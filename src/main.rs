//! mine.rs

#[macro_use]
extern crate rocket;

#[macro_use]
extern crate log;

use rocket::form::Form;
use rocket::fs::{FileServer, NamedFile};
use rocket::http::{ContentType, Method, Status};
use rocket::request::{FromRequest, Outcome, Request};
use rocket::response::{self, Responder, Response};
use rocket_cors::{AllowedOrigins, CorsOptions};
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};
use tempfile::Builder;

/// Наш «guard» для проверки заголовка Authorization
pub struct ApiKey(pub String);

#[rocket::async_trait]
impl<'r> FromRequest<'r> for ApiKey {
    type Error = ();

    async fn from_request(req: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        // Собираем все заголовки Authorization
        let auth_headers: Vec<_> = req.headers().get("Authorization").collect();
        if auth_headers.len() != 1 {
            return Outcome::Error((Status::Unauthorized, ()));
        }
        let provided = auth_headers[0];

        // Ожидаемый формат «Bearer <токен>» берём из переменной окружения API_KEY
        let expected = std::env::var("API_KEY")
            .map(|v| format!("Bearer {}", v))
            .unwrap_or_default();

        if provided == expected {
            Outcome::Success(ApiKey(provided.to_string()))
        } else {
            Outcome::Error((Status::Unauthorized, ()))
        }
    }
}

#[derive(FromFormField)]
enum PdfEngine {
    Weasyprint,
    Wkhtmltopdf,
    Pdflatex,
}

impl std::fmt::Display for PdfEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            PdfEngine::Weasyprint => write!(f, "weasyprint"),
            PdfEngine::Wkhtmltopdf => write!(f, "wkhtmltopdf"),
            PdfEngine::Pdflatex => write!(f, "pdflatex"),
        }
    }
}

#[derive(FromForm)]
struct ConvertForm {
    markdown: String,
    css: Option<String>,
    engine: Option<PdfEngine>,
}

#[derive(Debug)]
enum ConvertError {
    Output(Output),
    IO(io::Error),
}

impl From<io::Error> for ConvertError {
    fn from(err: io::Error) -> ConvertError {
        ConvertError::IO(err)
    }
}

impl<'r> Responder<'r, 'static> for ConvertError {
    fn respond_to(self, _: &Request<'_>) -> response::Result<'static> {
        let mut builder = Response::build();
        match self {
            ConvertError::Output(output) => {
                builder
                    .header(ContentType::Plain)
                    .sized_body(output.stderr.len(), io::Cursor::new(output.stderr))
                    .status(Status::BadRequest);
            }
            ConvertError::IO(_) => {
                builder.status(Status::InternalServerError);
            }
        }
        builder.ok()
    }
}

/// Сам хэндлер конвертации: note, первым аргументом теперь идёт ApiKey
#[post("/convert", data = "<form>")]
async fn convert(_key: ApiKey, form: Form<ConvertForm>) -> Result<NamedFile, ConvertError> {
    // создаём временный PDF-файл
    let pdf_temp_path = Builder::new()
        .suffix(".pdf")
        .tempfile()
        .map_err(ConvertError::IO)?
        .into_temp_path();
    let pdf_path = pdf_temp_path.to_str().unwrap();

    let mut pandoc = Command::new("pandoc");
    pandoc.arg(format!("--output={}", pdf_path));
    pandoc.arg(format!(
        "--pdf-engine={}",
        form.engine
            .as_ref()
            .unwrap_or(&PdfEngine::Weasyprint)
            .to_string()
    ));

    // опциональный CSS
    if let Some(css_content) = &form.css {
        let mut css_file = Builder::new()
            .suffix(".css")
            .tempfile()
            .map_err(ConvertError::IO)?;
        css_file.write_all(css_content.as_bytes()).map_err(ConvertError::IO)?;
        pandoc.arg(format!("--css={}", css_file.path().display()));
    }

    // передаём markdown на stdin
    pandoc.stdin(Stdio::piped());
    pandoc.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = pandoc.spawn().map_err(ConvertError::IO)?;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(form.markdown.as_bytes())
        .map_err(ConvertError::IO)?;
    let output = child.wait_with_output().map_err(ConvertError::IO)?;

    if !output.status.success() {
        return Err(ConvertError::Output(output));
    }

    NamedFile::open(Path::new(pdf_path)).await.map_err(ConvertError::IO)
}

#[launch]
fn rocket() -> _ {
    // CORS по аналогии с вашим примером
    let cors = CorsOptions::default()
        .allowed_origins(AllowedOrigins::all())
        .allowed_methods(
            vec![Method::Get, Method::Post, Method::Options]
                .into_iter()
                .map(From::from)
                .collect(),
        )
        .to_cors()
        .expect("CORS config");

    rocket::build()
        .attach(cors)
        .mount("/", routes![convert])
        .mount("/", FileServer::from("static"))
}
