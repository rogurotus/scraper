
#[macro_use]
extern crate lazy_static;
extern crate chrono;
use chrono::prelude::DateTime;
use chrono::Utc;
use std::time::{Instant, UNIX_EPOCH, Duration};
use std::error::Error;
use hyper::body::HttpBody as _;
use hyper::client::{Client, HttpConnector};
use hyper::Uri;
use scraper::{Html, Selector};
use dashmap::DashMap;
use rusqlite::named_params;
use std::fs::File;
use std::io::prelude::*;

lazy_static! 
{
    static ref TAG_CACH: DashMap<String, i32> = DashMap::new();
    static ref CLIENT: Client<HttpConnector> = hyper::Client::new();
}

#[derive(Debug, Default)]
struct Note
{
    link: String,
    rating: f32,
    tags: Vec<String>,
    date_time: String,
}

async fn get_page(client: &Client<HttpConnector>, host: &str, tag: &str, page: i32) -> Result<String, Box<dyn Error>>
{
    let link = format!("{}/{}/{}", host, tag, page);
    let mut res = client.get(link.parse::<Uri>()?).await?;
    let mut vec: Vec<u8> = vec![];
    while let Some(chunk) = res.body_mut().data().await
    {
        let mut v = chunk?.to_vec();
        vec.append(&mut v);
    }
    Ok(String::from_utf8_lossy(&vec).to_string())
}

fn page_to_vec_note(page: &str) -> Vec<Note>
{
    let mut vec = vec![];
    let document = Html::parse_document(page);
    for post in document.select(&Selector::parse("div.postContainer").unwrap())
    {
        let mut note = Note::default();

        note.date_time = post
            .select(&Selector::parse("span[data-time]").unwrap())
            .next().unwrap()
            .value().attrs()
            .filter(|(_, b)| b.parse::<u64>().is_ok())
            .map(
                |(_, b)| 
                {
                    let d = UNIX_EPOCH + Duration::from_secs(b.parse::<u64>().unwrap());
                    let datetime = DateTime::<Utc>::from(d);
                    let timestamp_str = datetime.format("%Y-%m-%d").to_string();
                    timestamp_str.to_string()
                })
            .next().unwrap();

        note.tags = post
            .select(&Selector::parse("h2.taglist").unwrap())
            .next().unwrap()
            .select(&Selector::parse("a").unwrap())
            .map(|a| 
                {
                    a
                    .value().attrs()
                    .filter(|(a, _)| *a == "title")
                    .map(|(_, b)| b.to_string()).next().unwrap()
                })
            .collect();

        note.rating = post
            .select(&Selector::parse("span.post_rating").unwrap())
            .next().unwrap()
            .select(&Selector::parse("span").unwrap())
            .next().unwrap().inner_html()
            .split("<").next().unwrap_or("0.0")
            .parse::<f32>().unwrap_or(0.0);

        note.link = post
            .select(&Selector::parse("a.link").unwrap())
            .next().unwrap()
            .value().attrs()
            .filter(|(a, _)| *a == "href")
            .map(|(_, b)| b.to_string())
            .next().unwrap_or("".to_string());

        vec.push(note);
    }
    vec
}

fn write_in_bd(posts: Vec<Note>, host: &str) -> Result<(), Box<dyn Error>>
{
    let mut conn = rusqlite::Connection::open("posts.db").expect("WTF!");
    let transaction = conn.transaction()?;
    for note in &posts
    {
        let last_id_post: String;
        match transaction.execute("insert into post (link, rating, host, date_time) values (?1, ?2, ?3, ?4)", 
            &[&note.link, &note.rating.to_string(), host, &note.date_time])
        {
            Ok(_) => last_id_post = transaction.last_insert_rowid().to_string(),
            _ => continue,
        }
        for ref tag in &note.tags
        {
            let last_id_tag : String = 
            if TAG_CACH.contains_key(&tag.to_lowercase())
            {
                (*TAG_CACH.get(&tag.to_lowercase()).unwrap()).to_string()
            }
            else
            {
                match transaction.execute("insert into tag (name) values (?1)", &[&tag.to_lowercase()])
                {
                    Ok(_) => transaction.last_insert_rowid().to_string(),
                    Err(_) =>
                    {
                        let mut tag_id = transaction.prepare("select id from tag where name = :name")?;
                        let mut rows = tag_id.query_named(named_params!{ ":name": &tag.to_lowercase() })?;
                        if let Some(row) = rows.next()?
                        {
                            let id : i32 = row.get(0)?;
                            TAG_CACH.insert(tag.to_lowercase().to_string(), id);
                            id.to_string()
                        }
                        else
                        {
                            continue;
                        }
                    },
                }
            };
            match transaction.execute("insert into post_tag (post, tag) values (?1, ?2)",
                 &[&last_id_post, &last_id_tag])
            {
                Ok(_) => {},
                Err(e) => println!("{} - {} ERROR {}", &last_id_post, &last_id_tag, e),
            }
        }
    }
    transaction.commit()?;
    Ok(())
}

fn read_config(file: &mut File) -> Vec<(String, i32)>
{
    let mut s = String::new();

    match file.read_to_string(&mut s) 
    {
        Err(why) => panic!("couldn't read: {}", why),
        Ok(_) => (),
    }

    s
        .replace("\u{feff}", "")
        .split_ascii_whitespace()
        .map(
            |a| 
            {
                a.split(";")
                    .map(|a| a.to_owned())
                    .collect::<Vec<String>>()
            })
        .map(
            |a| 
            {
                let mut iter = a.into_iter();
                (iter.next().unwrap(), iter.next().unwrap().parse::<i32>().unwrap())
            }
        ).collect()
}

fn next(page: &str) -> bool
{
    let document = Html::parse_document(page);
    document.select(&Selector::parse("a.prev").unwrap()).count() != 0
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>>
{
    let time = Instant::now();

    let mut file = match File::open("config.txt") 
    {
        Err(why) => panic!("couldn't open: {}", why),
        Ok(file) => file,
    };

    let confs = read_config(&mut file);
    drop(file);

    let mut new_confs = vec![];

    for (tag, page) in confs
    {
        let mut page = page;
        loop
        {
            let wait = tokio::time::sleep(Duration::from_millis(1010));
            let time_ = Instant::now();
            if let Ok(html) = get_page(&CLIENT, "http://joyreactor.cc", &tag, page).await
            {
                let notes = page_to_vec_note(&html);
                write_in_bd(notes, "http://joyreactor.cc").expect("write db fail");

                println!("{} {} {}", tag, page, time_.elapsed().as_millis());

                if !next(&html)
                {
                    new_confs.push((tag, page));
                    break;
                }
            }
            else
            {
                new_confs.push((tag, page));
                break;
            }
            page += 1;
            wait.await;
        }
    }

    let mut file = match File::create("config.txt") 
    {
        Err(why) => panic!("couldn't open: {}", why),
        Ok(file) => file,
    };

    for (t,p) in new_confs
    {
        write!(file, "{};{}\r\n", t, p).expect("write config fail");
    }

    let hours = time.elapsed().as_secs() / 3600;
    let mins = (time.elapsed().as_secs() - hours * 3600) / 60;
    let secs = time.elapsed().as_secs() % (3600 * 60);

    println!("done {}:{}:{}", hours, mins, secs);
    Ok(())
}