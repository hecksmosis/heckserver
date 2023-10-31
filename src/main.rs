use axum::{
    body::Body,
    extract::{Query, State},
    http::Request,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use axum_macros::debug_handler;
use sqlx::{Pool, Sqlite};
use static_handler::static_handler;
use std::net::SocketAddr;
use tower::ServiceExt;
use tower_http::services::ServeFile;
use tracing::debug;

const PORT: u16 = 3000;
const WORD_TYPES: [&str; 3] = ["noun", "verb", "amuini"];

mod static_handler;

#[derive(Clone)]
struct App {
    pub db: sqlx::Pool<Sqlite>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();
    let app = App {
        db: sqlx::sqlite::SqlitePoolOptions::new()
            .connect("sqlite:./tavsa.db")
            .await
            .unwrap(),
    };

    let app = Router::new()
        .route("/tavsa", get(tavsa))
        .route("/tavsa/add", post(tavsa_add))
        .route("/tavsa/dict", get(tavsa_dict))
        .route("/tavsa/delete", post(tavsa_delete))
        .route("/tavsa/serialize", post(tavsa_serialize))
        .route("/game", get(game))
        .nest_service("/static", get(static_handler))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(app);

    let addr = SocketAddr::from(([127, 0, 0, 1], PORT));
    debug!("listening on {}", addr);

    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .await
        .unwrap();
}

async fn tavsa() -> impl IntoResponse {
    let req = Request::builder()
        .uri("/tavsa")
        .body(Body::empty())
        .unwrap();

    ServeFile::new("static/tavsa.html")
        .oneshot(req)
        .await
        .unwrap()
}

#[derive(serde::Deserialize, Debug)]
struct TavsaAdd {
    pub word_type: String,
    pub word: String,
    pub definition: String,
}

struct TavsaWord {
    pub id: i64,
    pub word: String,
    pub definition: String,
    pub r#type: String,
    pub etymology: Option<String>,
}

#[debug_handler]
async fn tavsa_add(
    State(db): State<App>,
    Json(TavsaAdd {
        mut word_type,
        word,
        definition,
    }): Json<TavsaAdd>,
) -> impl IntoResponse {
    if word_type.is_empty() || word.is_empty() || definition.is_empty() {
        return "Missing fields".to_string();
    }

    // TODO: Spellcheck
    if word_type == "auto" {
        if word.contains("rn") {
            word_type = "verb".to_string();
        } else {
            return "Invalid word type".to_string();
        }
    }

    if WORD_TYPES.iter().find(|&x| x == &word_type).is_none() {
        return "Invalid word type".to_string();
    }

    let res = sqlx::query!(
        "INSERT INTO tavsa (type, word, definition) VALUES (?, ?, ?)",
        word_type,
        word,
        definition
    )
    .execute(&db.db)
    .await;

    match res {
        Ok(_) => get_words(&db.db).await,
        Err(err) => err.to_string(),
    }
}

#[derive(serde::Deserialize, Debug)]
struct TavsaSerialize {
    pub word_type: String,
}

#[debug_handler]
async fn tavsa_serialize(
    State(db): State<App>,
    Query(q): Query<TavsaSerialize>,
    body: String,
) -> impl IntoResponse {
    let first_index = match sqlx::query!("select id from tavsa order by id desc limit 1")
        .fetch_one(&db.db)
        .await
    {
        Ok(row) => row.id + 1,
        Err(_) => 1,
    };

    let words = body.split_whitespace().collect::<Vec<&str>>();
    let tavsa_words = words.iter().enumerate().map(|(index, word)| {
        let pair = word.split("=").collect::<Vec<_>>();
        debug!("{:?}", pair);
        let etymology = get_word_etymology(&pair[1], &q.word_type);

        TavsaWord {
            id: index as i64 + first_index,
            word: pair[1].to_string().to_lowercase(),
            definition: pair[0].to_string().to_lowercase(),
            r#type: q.word_type.clone(),
            etymology: None,
        }
    });

    let mut query_string = tavsa_words.fold(
        "INSERT INTO tavsa (id, type, word, definition) VALUES".to_string(),
        |accum, word| {
            format!(
                "{} ({}, '{}', '{}', '{}'),",
                accum, word.id, word.r#type, word.word, word.definition
            )
        },
    );

    query_string.pop();
    query_string += ";";

    sqlx::query(&query_string).execute(&db.db).await.unwrap();
}

struct TavsaEtymology {
    pub lexema: String,
    pub subject: Option<String>,
    pub ci: Option<String>,
    pub modifiers: Vec<String>,
}

fn get_word_etymology(word: &str, word_type: &str) -> Option<TavsaEtymology> {
    match word_type {
        "verb" => {
            if let Some(index) = word.find("rn") {
                let letters = &word[..index];
                let subject = word.find("t").or(word.find("d")).map(|index| word[index..=index+1].to_string());
                let ci = word.find("k").or(word.find("g")).map(|index| word[index..=index+1].to_string());

                Some(TavsaEtymology {
                    lexema: letters.to_string(),
                    subject,
                    ci,
                    modifiers: vec![],
                })
            } else {
                None
            }
        }
        "noun" => todo!(),
        "amuini" => todo!(),
        _ => todo!(),
    }
}

async fn tavsa_dict(State(db): State<App>) -> impl IntoResponse {
    get_words(&db.db).await
}

#[derive(serde::Deserialize, Debug)]
struct TavsaDel {
    pub id: String,
}

#[debug_handler]
async fn tavsa_delete(
    State(db): State<App>,
    Json(TavsaDel { id }): Json<TavsaDel>,
) -> impl IntoResponse {
    let res = sqlx::query!("DELETE FROM tavsa WHERE id = ?", id)
        .execute(&db.db)
        .await;

    match res {
        Ok(_) => get_words(&db.db).await,
        Err(err) => err.to_string(),
    }
}

const DELETE_COMPONENT: &str = r#"<form hx-post="/tavsa/delete" hx-ext='json-enc' hx-target=" #dict" hx-swap="innerHTML">
    <input type="hidden" name="id" value="{}">
    <input type="submit" value="Delete"></form>"#;

async fn get_words(db: &Pool<Sqlite>) -> String {
    let dictionary = sqlx::query_as!(TavsaWord, "SELECT * FROM tavsa")
        .fetch_all(db)
        .await
        .unwrap();

    dictionary.iter().fold(String::new(), |accum, word| {
        format!(
            "{}<div>{}: {} {}</div>",
            accum,
            word.word,
            word.definition,
            DELETE_COMPONENT.replace("{}", &word.id.to_string())
        )
    })
}

async fn game() -> impl IntoResponse {
    let req = Request::builder()
        .uri("private/game.html")
        .body(Body::empty())
        .unwrap();

    ServeFile::new("private/game.html")
        .oneshot(req)
        .await
        .unwrap()
}
