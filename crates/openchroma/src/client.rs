//! Minimal client for the local control API, shared by the CLI and the
//! desktop app.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use serde_json::Value;

use crate::config::UI_PORT;

#[derive(Debug)]
pub enum Error {
    /// Nothing is listening: the service is not running.
    Offline,
    /// The service answered with an error, or the exchange failed.
    Failed(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Offline => f.write_str("the OpenChroma service is not running"),
            Error::Failed(e) => f.write_str(e),
        }
    }
}

/// One request over a fresh HTTP/1.0 connection. `client` names the caller in
/// the `X-OpenChroma` header that state-changing requests require.
pub fn request(client: &str, method: &str, path: &str, body: Option<&Value>) -> Result<Value, Error> {
    let mut stream = TcpStream::connect(("127.0.0.1", UI_PORT)).map_err(|_| Error::Offline)?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let body = body.map(Value::to_string).unwrap_or_default();
    let req = format!(
        "{method} {path} HTTP/1.0\r\nHost: 127.0.0.1\r\nX-OpenChroma: {client}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let fail = |e: std::io::Error| Error::Failed(e.to_string());
    stream.write_all(req.as_bytes()).map_err(fail)?;
    let mut resp = String::new();
    stream.read_to_string(&mut resp).map_err(fail)?;
    let (head, body) = resp.split_once("\r\n\r\n").ok_or_else(|| Error::Failed("malformed response".into()))?;
    let value: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    if !head.split_whitespace().nth(1).is_some_and(|c| c.starts_with('2')) {
        let msg = value["error"].as_str().map(str::to_owned).unwrap_or_else(|| head.lines().next().unwrap_or("request failed").to_owned());
        return Err(Error::Failed(msg));
    }
    Ok(value)
}
