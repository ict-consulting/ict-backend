use std::convert::TryFrom;
use std::fmt::Write;
use std::str::FromStr;

use actix_identity::Identity;
use futures::future;
use futures::TryFutureExt;
use pulldown_cmark as md;
use tokio::fs;
use tokio_postgres as psql;

use crate::error::{Error, Result};
use crate::i18n::Language;
use crate::path::PublicPath;

#[derive(Debug, Clone)]
enum Pattern {
    Empty,
    Login,
    Editor,
    Admin,
    Drafts,
    AdminPanel,
    Me(String),
    Path(String),
    Positional(usize),
    L10n(String),
    ArticlePositional(usize),
    PreviewLatest(usize),
    ArticleLatest(usize),
    PreviewTitle(String),
    ArticleTitle(String),
    Maybe(Box<Pattern>),
}

async fn author(client: &psql::Client, uid: i32) -> Result<Option<String>> {
    let user = client
        .query_opt(
            "select firstname, lastname, username from users where id = $1",
            &[&uid],
        )
        .await?;
    match user {
        Some(user) => {
            let firstname = user.get::<_, Option<&str>>("firstname");
            let lastname = user.get::<_, Option<&str>>("lastname");
            let username = user.get::<_, &str>("username");
            match (firstname, lastname) {
                (Some(first), Some(last)) => {
                    Ok(Some(format!("{} \"{}\" {}", first, username, last)))
                }
                (Some(first), None) => Ok(Some(format!("{} \"{}\"", first, username))),
                (None, Some(last)) => Ok(Some(format!("\"{}\" {}", username, last))),
                _ => Ok(Some(username.to_string())),
            }
        }
        None => Ok(None),
    }
}

impl FromStr for Pattern {
    type Err = Error;

    fn from_str(pattern: &str) -> Result<Self> {
        if pattern.is_empty() {
            Ok(Pattern::Empty)
        } else if pattern == "login" {
            Ok(Pattern::Login)
        } else if pattern == "editor" {
            Ok(Pattern::Editor)
        } else if pattern == "admin" {
            Ok(Pattern::Admin)
        } else if pattern == "drafts" {
            Ok(Pattern::Drafts)
        } else if pattern == "admin-panel" {
            Ok(Pattern::AdminPanel)
        } else if pattern.starts_with("me.") {
            Ok(Pattern::Me(pattern[3..].to_string()))
        } else if pattern.starts_with('/') {
            Ok(Pattern::Path(pattern[1..].to_string()))
        } else if pattern.starts_with('%') {
            Ok(Pattern::Positional(pattern[1..].parse()?))
        } else if pattern.starts_with("l10n(") {
            let start = "l10n(".len();
            let end = pattern.len() - 1;
            if &pattern[end..] != ")" {
                return Err(Error::InvalidPattern(pattern.to_string()));
            }
            let sub = &pattern[start..end];
            Ok(Pattern::L10n(sub.to_string()))
        } else if pattern.starts_with("article%") {
            Ok(Pattern::ArticlePositional(
                pattern["article%".len()..].parse()?,
            ))
        } else if pattern.starts_with("preview~") {
            Ok(Pattern::PreviewLatest(pattern["preview~".len()..].parse()?))
        } else if pattern.starts_with("article~") {
            Ok(Pattern::ArticleLatest(pattern["article~".len()..].parse()?))
        } else if pattern.starts_with("preview ") {
            Ok(Pattern::PreviewTitle(
                pattern["preview ".len()..].to_string(),
            ))
        } else if pattern.starts_with("article ") {
            Ok(Pattern::ArticleTitle(
                pattern["article ".len()..].to_string(),
            ))
        } else if pattern.starts_with("maybe(") {
            let start = "maybe(".len();
            let end = pattern.len() - 1;
            if &pattern[end..] != ")" {
                return Err(Error::InvalidPattern(pattern.to_string()));
            }
            let sub = &pattern[start..end];
            Ok(Pattern::Maybe(Box::new(sub.parse()?)))
        } else {
            Err(Error::InvalidPattern(pattern.to_string()))
        }
    }
}

impl Pattern {
    pub async fn to_string_nonrecursive(
        self,
        identity: &Identity,
        client: &psql::Client,
        lang: &Language,
        args: &[String],
    ) -> Result<String> {
        match self {
            Pattern::Empty => Ok(String::new()),
            Pattern::Login => {
                match identity.identity() {
                    Some(identity) => {
                        Ok(format!("<span class=\"float-right\"><a href=\"/auth/logout.html\">{{{{{{l10n(logout)}}}}}}</a></span> \
                                    <span class=\"float-right\"><a href=\"/account/me.html\">{{{{{{l10n(logged_in_as)}}}}}}: {}</a></span>", identity))
                    }
                    None => {
                        Ok("<span class=\"float-right\"><a href=\"/login.html\">{{{l10n(login)}}}</a></span> \
                            <span class=\"float-right\"><a href=\"/create.html\">{{{l10n(register)}}}</a></span>".to_string())
                    }
                }
            }
            Pattern::Editor => {
                match identity.identity() {
                    Some(identity) => {
                        // only employees are allowed to make new articles
                        let user = client.query_opt(
                            "select employees.id from employees where employees.uid = \
                             (select users.id as uid from users where username = $1)",
                            &[&identity]
                        ).await?;
                        if user.is_some() {
                            Ok("<span class=\"float-right\"><a href=\"/account/editor.html\">{{{l10n(new_article)}}}</a></span>".to_string())
                        } else {
                            Err(Error::AuthorizationFailed)
                        }
                    }
                    None => {
                        Err(Error::AuthorizationFailed)
                    }
                }
            }
            Pattern::Admin => {
                match identity.identity() {
                    Some(identity) => {
                        // only employees are allowed to make new articles
                        let user = client.query_opt(
                            "select admins.id from admins where admins.uid = \
                             (select users.id as uid from users where username = $1)",
                            &[&identity]
                        ).await?;
                        if user.is_some() {
                            Ok("<span class=\"float-right\"><a href=\"/account/admin.html\">{{{l10n(admin_panel)}}}</a></span>".to_string())
                        } else {
                            Err(Error::AuthorizationFailed)
                        }
                    }
                    None => {
                        Err(Error::AuthorizationFailed)
                    }
                }
            }
            Pattern::Drafts => {
                match identity.identity() {
                    Some(identity) => {
                        let drafts = client.query(
                            "select id, path, title from drafts where drafts.author = \
                             (select users.id as author from users where username = $1)",
                            &[&identity]
                        ).await?;
                        if drafts.len() > 0 {
                            let mut select = format!("<select oninput=\"load_draft()\" id=\"draft-select\" name=\"draft-select\" size=\"{}\">\n", drafts.len().min(5).max(2));
                            for draft in drafts {
                                let value = draft.get::<_, i32>("id");
                                let mut title = draft.get::<_, Option<&str>>("title").unwrap_or("&lt;untitled&gt;");
                                if title.is_empty() {
                                    title = "&lt;untitled&gt;";
                                }
                                write!(select, "<option value=\"{}\">{}</option>\n", value, title).expect("couldn't write to string");
                            }
                            write!(select, "</select>\n").expect("couldn't write to string");
                            Ok(select)
                        } else {
                            Ok(String::new())
                        }
                    }
                    None => {
                        Err(Error::AuthorizationFailed)
                    }
                }
            }
            Pattern::AdminPanel => {
                match identity.identity() {
                    Some(identity) => {
                        let admin = client.query_opt(
                            "select id from admins where uid = \
                             (select id as uid from users where username = $1)",
                            &[&identity]
                        ).await?;
                        if admin.is_none() {
                            return Err(Error::AuthorizationFailed);
                        }

                        let users = client.query(
                            "select id, username, firstname, lastname, email from users",
                            &[]
                        ).await?;
                        let mut select = format!("<table>\n");
                        write!(select, "<tr>\n").expect("couldn't write to string");
                        write!(select, "<th>UID</th>\n").expect("couldn't write to string");
                        write!(select, "<th>{{{{{{l10n(account_username)}}}}}}</th>\n").expect("couldn't write to string");
                        write!(select, "<th>{{{{{{l10n(account_firstname)}}}}}}</th>\n").expect("couldn't write to string");
                        write!(select, "<th>{{{{{{l10n(account_lastname)}}}}}}</th>\n").expect("couldn't write to string");
                        write!(select, "<th>{{{{{{l10n(account_email)}}}}}}</th>\n").expect("couldn't write to string");
                        write!(select, "<th>{{{{{{l10n(account_isemployee)}}}}}}</th>\n").expect("couldn't write to string");
                        write!(select, "<th>{{{{{{l10n(account_isadmin)}}}}}}</th>\n").expect("couldn't write to string");
                        write!(select, "</tr>\n").expect("couldn't write to string");
                        for user in users {
                            let id = user.get::<_, i32>("id");
                            let isadmin = client
                                .query_opt(
                                    "select id from admins where uid = \
                                    (select id as uid from users where id = $1)",
                                    &[&id]
                                )
                                .await?
                                .is_some();
                            let isemployee = client
                                .query_opt(
                                    "select id from employees where uid = \
                                    (select id as uid from users where id = $1)",
                                    &[&id]
                                )
                                .await?
                                .is_some();
                            let isadmin = if isadmin { "checked=\"checked\"" } else { "" };
                            let isemployee = if isemployee { "checked=\"checked\"" } else { "" };
                            write!(select, "<tr>\n").expect("couldn't write to string");
                            write!(select, "<td>{}</td>\n", id).expect("couldn't write to string");
                            write!(select, "<td>{}</td>\n", user.get::<_, &str>("username")).expect("couldn't write to string");
                            write!(select, "<td>{}</td>\n", user.get::<_, Option<&str>>("firstname").unwrap_or("")).expect("couldn't write to string");
                            write!(select, "<td>{}</td>\n", user.get::<_, Option<&str>>("lastname").unwrap_or("")).expect("couldn't write to string");
                            write!(select, "<td><a href=\"mailto:{0}\">{0}</a></td>\n", user.get::<_, &str>("email")).expect("couldn't write to string");
                            write!(select, "<td><form><input type=\"checkbox\" {} oninput=\"make_employee(this, {})\"/></form></td>\n", isemployee, id).expect("couldn't write to string");
                            write!(select, "<td><form><input type=\"checkbox\" {} oninput=\"make_admin(this, {})\"/></form></td>\n", isadmin, id).expect("couldn't write to string");
                            write!(select, "</tr>\n").expect("couldn't write to string");
                        }
                        write!(select, "</table>\n").expect("couldn't write to string");
                        Ok(select)
                    }
                    None => {
                        Err(Error::AuthorizationFailed)
                    }
                }
            }
            Pattern::Me(field) => {
                if field == "pwhash" {
                    Ok("No passwords for you!".to_string())
                } else {
                    match identity.identity() {
                        Some(me) => {
                            match client.query_opt("select * from users where username = $1", &[&me]).await? {
                                Some(row) => Ok(row.get::<&str, &str>(&field).to_string()),
                                None => Ok("".to_string()),
                            }
                        }
                        None => Ok("".to_string()),
                    }
                }
            }
            Pattern::Path(path) => {
                let path = PublicPath::try_from(path)?;
                let text = fs::read_to_string(&path).await?;
                if path.extension() == Some("md".as_ref()) {
                    let parser = md::Parser::new_ext(&text, md::Options::all());
                    let mut html = String::new();
                    md::html::push_html(&mut html, parser);
                    Ok(html)
                } else {
                    Ok(text)
                }
            }
            Pattern::Positional(pos) => {
                let path = args
                    .get(pos - 1)
                    .ok_or_else(|| Error::ResourceNotFound(format!("%{}", pos)))?;
                let path = PublicPath::try_from(&**path)?;
                let text = fs::read_to_string(&path).await?;
                if path.extension() == Some("md".as_ref()) {
                    let parser = md::Parser::new_ext(&text, md::Options::all());
                    let mut html = String::new();
                    md::html::push_html(&mut html, parser);
                    Ok(html)
                } else {
                    Ok(text)
                }
            }
            Pattern::L10n(key) => {
                Ok(lang[&key].to_string())
            }
            Pattern::ArticlePositional(pos) => {
                let path = args
                    .get(pos - 1)
                    .ok_or_else(|| Error::ResourceNotFound(format!("%{}", pos)))?;
                let args: &[&(dyn psql::types::ToSql + Sync)] = &[path];
                let article = client
                    .query_one("select title, to_char(cdate, 'yyyy-mm-dd') as date, author from articles where path = $1", args);
                let contents = article
                    .map_err(From::from)
                    .and_then(async move |article| {
                        let path = PublicPath::try_from(&**path)?;
                        if path.exists() {
                            let text = fs::read_to_string(&path)
                                .await?;
                            if path.extension() == Some("md".as_ref()) {
                                let parser = md::Parser::new_ext(&text, md::Options::all());
                                let mut html = String::new();
                                md::html::push_html(&mut html, parser);
                                Ok((article, html))
                            } else {
                                Ok((article, text))
                            }
                        } else {
                            Err(Error::ResourceNotFound(path.to_string_lossy().to_string()))
                        }
                    });
                contents.and_then(async move |(article, contents)| {
                    let by_author = author(client, article.get::<_, i32>("author")).await?.map(|author| format!(" {{{{{{l10n(by_author)}}}}}} {}", author)).unwrap_or_else(String::new);
                    Ok(format!(
                        "<article><h1>{}</h1>{}{}<br/>{}</article>",
                        article.get::<_, &str>("title"),
                        article.get::<_, &str>("date"),
                        by_author,
                        contents,
                    ))
                }).await
            }
            Pattern::PreviewLatest(no) => {
                let rows = client
                    .query("select title, path, to_char(cdate, 'yyyy-mm-dd') as date, author from articles order by cdate", &[])
                    .await?;
                let article = rows.len().checked_sub(no).and_then(|no| rows.get(no)).ok_or_else(|| Error::ResourceNotFound(format!("preview~{}", no)))?;
                let by_author = author(client, article.get::<_, i32>("author")).await?.map(|author| format!(" {{{{{{l10n(by_author)}}}}}} {}", author)).unwrap_or_else(String::new);
                Ok(format!(
                    "<article><h2><a href=\"{}\">{}</a></h2>{}{}</article>",
                    article.get::<_, &str>("path"),
                    article.get::<_, &str>("title"),
                    article.get::<_, &str>("date"),
                    by_author,
                ))
            }
            Pattern::ArticleLatest(no) => {
                let rows = client
                    .query("select path, title, to_char(cdate, 'yyyy-mm-dd') as date, author from articles order by cdate", &[])
                    .await?;
                let article = rows.len().checked_sub(no).and_then(|no| rows.get(no));
                let contents = article.map(|article| {
                    future::ok(article)
                        .and_then(async move |article| {
                            let path = article.get::<_, &str>("path");
                            let path = PublicPath::try_from(path)?;
                            if path.exists() {
                                let text = fs::read_to_string(&path)
                                    .await?;
                                if path.extension() == Some("md".as_ref()) {
                                    let parser = md::Parser::new_ext(&text, md::Options::all());
                                    let mut html = String::new();
                                    md::html::push_html(&mut html, parser);
                                    Ok((article, html))
                                } else {
                                    Ok((article, text))
                                }
                            } else {
                                Err(Error::ResourceNotFound(path.to_string_lossy().to_string()))
                            }
                        })
                });
                if let Some(contents) = contents {
                    contents.and_then(async move |(article, contents)| {
                        let by_author = author(client, article.get::<_, i32>("author")).await?.map(|author| format!(" {{{{{{l10n(by_author)}}}}}} {}", author)).unwrap_or_else(String::new);
                        Ok(format!(
                            "<article><h1>{}</h1>{}{}<br/>{}</article>",
                            article.get::<_, &str>("title"),
                            article.get::<_, &str>("date"),
                            by_author,
                            contents,
                        ))
                    }).await
                } else {
                    Ok(String::new())
                }
            }
            Pattern::PreviewTitle(title) => {
                let article = client
                    .query_one("select title, path, to_char(cdate, 'yyyy-mm-dd') as date, author from articles where title = $1", &[&title])
                    .await?;
                let by_author = author(client, article.get::<_, i32>("author")).await?.map(|author| format!(" {{{{{{l10n(by_author)}}}}}} {}", author)).unwrap_or_else(String::new);
                Ok(format!(
                    "<article><h2><a href=\"{}\">{}</a></h2>{}{}</article>",
                    article.get::<_, &str>("path"),
                    article.get::<_, &str>("title"),
                    article.get::<_, &str>("date"),
                    by_author,
                ))
            }
            Pattern::ArticleTitle(title) => {
                let args: &[&(dyn psql::types::ToSql + Sync)] = &[&title];
                let article = client
                    .query_one("select title, to_char(cdate, 'yyyy-mm-dd') as date, author from articles where title = $1", args);
                let contents = article
                    .map_err(From::from)
                    .and_then(async move |article| {
                        let path = article.get::<_, &str>("path");
                        let path = PublicPath::try_from(path)?;
                        if path.exists() {
                            let text = fs::read_to_string(&path)
                                .await?;
                            if path.extension() == Some("md".as_ref()) {
                                let parser = md::Parser::new_ext(&text, md::Options::all());
                                let mut html = String::new();
                                md::html::push_html(&mut html, parser);
                                Ok((article, html))
                            } else {
                                Ok((article, text))
                            }
                        } else {
                            Err(Error::ResourceNotFound(path.to_string_lossy().to_string()))
                        }
                    });
                contents.and_then(async move |(article, contents)| {
                    let by_author = author(client, article.get::<_, i32>("author")).await?.map(|author| format!(" {{{{{{l10n(by_author)}}}}}} {}", author)).unwrap_or_else(String::new);
                    Ok(format!(
                        "<article><h1>{}</h1>{}{}<br/>{}</article>",
                        article.get::<_, &str>("title"),
                        article.get::<_, &str>("date"),
                        by_author,
                        contents,
                    ))
                }).await
            }
            Pattern::Maybe(_) => {
                Err(Error::AsyncRecursion)
            }
        }
    }

    pub async fn to_string(
        self,
        identity: &Identity,
        client: &psql::Client,
        lang: &Language,
        args: &[String],
    ) -> Result<String> {
        match self {
            Pattern::Maybe(opt) => Ok(opt
                .to_string_nonrecursive(identity, client, lang, args)
                .await
                .unwrap_or_else(|_| String::new())),
            other => {
                other
                    .to_string_nonrecursive(identity, client, lang, args)
                    .await
            }
        }
    }

    pub async fn replace_at(
        self,
        identity: &Identity,
        client: &psql::Client,
        lang: &Language,
        input: &mut String,
        start: usize,
        end: usize,
        args: &[String],
    ) -> Result<usize> {
        let text = self.to_string(identity, client, lang, args).await?;
        input.replace_range(start..(end + 3), &text);
        Ok(text.len())
    }
}

async fn replace_at(
    identity: &Identity,
    client: &psql::Client,
    lang: &Language,
    input: &mut String,
    start: usize,
    args: &[String],
) -> Result<usize> {
    if let Some(len) = &input[start..].find("}}}") {
        let end = start + len;
        let pattern = &input[(start + 3)..end];
        let pattern = pattern.parse().unwrap_or(Pattern::Empty);
        pattern
            .replace_at(identity, client, lang, input, start, end, args)
            .await
    } else {
        Ok(0)
    }
}

pub async fn search_replace(
    identity: &Identity,
    client: &psql::Client,
    lang: &Language,
    input: &mut String,
    args: &[String],
) -> Result<()> {
    let mut i = 0;
    loop {
        match input[i..].find("{{{") {
            Some(idx) => {
                let len = replace_at(identity, client, lang, input, idx, args).await?;
                i = idx + len;
            }
            None => break Ok(()),
        }
    }
}

pub async fn search_replace_recursive(
    identity: &Identity,
    client: &psql::Client,
    lang: &Language,
    input: &mut String,
    args: &[String],
) -> Result<()> {
    loop {
        match input.find("{{{") {
            Some(idx) => {
                replace_at(identity, client, lang, input, idx, args).await?;
            }
            None => break Ok(()),
        }
    }
}
