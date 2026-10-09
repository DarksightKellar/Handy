//! Jev (TypeSafe AI) decider: asks which of the options a speaker most likely
//! said in the blank of one sentence.
//!
//! API (checked against docs.typesafe.ai on 2026-10-09): `POST
//! https://api.typesafe.ai/v1/systemone` with `Authorization: Bearer <key>`.
//! The answer holds `answers.word.probabilities.option_i`. Errors are 401, 422,
//! 429 and 529.
//!
//! Only the one sentence is sent, as text. Every failure gives `None`, and the
//! learning layer then offers the other option in a notice instead. The log
//! holds status codes and error kinds only: never the sentence, never the key.

use std::sync::mpsc;
use std::time::Duration;

use log::warn;
use serde_json::{json, Value};

use super::core::learner::Decider;

pub const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
const MODEL: &str = "jev-latest";
const INSTRUCTIONS: &str = "The sentence was dictated and transcribed by speech recognition. \
    Which option did the speaker most likely say in the blank?";
/// Dictation must not wait on the network: past this, Handy pastes its best guess.
const TIMEOUT: Duration = Duration::from_millis(1500);

/// The request body. Option keys are `option_0`, `option_1`, …; the value is
/// the real wording.
pub fn build_request(sentence: &str, options: &[String]) -> Value {
    let criteria: serde_json::Map<String, Value> = options
        .iter()
        .enumerate()
        .map(|(i, option)| (format!("option_{i}"), json!(option)))
        .collect();
    json!({
        "model": MODEL,
        "state": sentence,
        "questions": {
            "word": {
                "type": "choice",
                "instructions": INSTRUCTIONS,
                "criteria": criteria,
            }
        }
    })
}

/// The probability of each of `n` options, in order. `None` when the answer
/// lacks one of them.
pub fn parse_probabilities(body: &Value, n: usize) -> Option<Vec<f64>> {
    let probabilities = body.pointer("/answers/word/probabilities")?;
    (0..n)
        .map(|i| probabilities.get(format!("option_{i}"))?.as_f64())
        .collect()
}

pub struct JevDecider {
    key: String,
    endpoint: String,
    timeout: Duration,
    client: reqwest::Client,
}

impl JevDecider {
    /// A decider for `key`, or `None` when the key is empty or the HTTP client
    /// cannot be built.
    pub fn new(key: &str) -> Option<Self> {
        Self::with_config(key, ENDPOINT, TIMEOUT)
    }

    pub fn with_config(key: &str, endpoint: &str, timeout: Duration) -> Option<Self> {
        let key = key.trim();
        if key.is_empty() {
            return None;
        }
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| warn!("Could not build the Jev HTTP client: {e}"))
            .ok()?;
        Some(Self {
            key: key.to_string(),
            endpoint: endpoint.to_string(),
            timeout,
            client,
        })
    }
}

/// One request. Returns the parsed body, or `None` after logging what kind of
/// failure it was.
async fn post(
    client: reqwest::Client,
    endpoint: String,
    key: String,
    body: Value,
) -> Option<Value> {
    let response = match client
        .post(endpoint)
        .bearer_auth(key)
        .json(&body)
        .send()
        .await
    {
        Ok(response) => response,
        Err(e) => {
            let kind = if e.is_timeout() {
                "timeout"
            } else if e.is_connect() {
                "connection"
            } else {
                "request"
            };
            warn!("Jev request failed ({kind})");
            return None;
        }
    };
    let status = response.status();
    if !status.is_success() {
        warn!("Jev answered with status {}", status.as_u16());
        return None;
    }
    match response.json::<Value>().await {
        Ok(body) => Some(body),
        Err(_) => {
            warn!("Jev answer was not valid JSON");
            None
        }
    }
}

impl Decider for JevDecider {
    /// Blocks until Jev answers or the timeout passes. Call it off the async
    /// runtime. The request runs on the app's runtime and this thread waits on
    /// a channel, so the call is safe from any thread that may block.
    fn choose(&self, sentence_with_blank: &str, options: &[String]) -> Option<Vec<f64>> {
        if options.len() < 2 {
            return None;
        }
        let body = build_request(sentence_with_blank, options);
        let (tx, rx) = mpsc::channel();
        let (client, endpoint, key) =
            (self.client.clone(), self.endpoint.clone(), self.key.clone());
        tauri::async_runtime::spawn(async move {
            let _ = tx.send(post(client, endpoint, key, body).await);
        });
        // The client enforces the timeout; the extra second only covers a task
        // that never gets to run.
        let answer = rx
            .recv_timeout(self.timeout + Duration::from_secs(1))
            .ok()??;
        let probabilities = parse_probabilities(&answer, options.len());
        if probabilities.is_none() {
            warn!("Jev answer had no probability for every option");
        }
        probabilities
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    fn options() -> Vec<String> {
        vec!["Eres".to_string(), "heiress".to_string()]
    }

    #[test]
    fn request_has_the_model_the_blank_and_numbered_options() {
        let body = build_request("push the fix to ___", &options());
        assert_eq!(body["model"], "jev-latest");
        assert_eq!(body["state"], "push the fix to ___");
        let question = &body["questions"]["word"];
        assert_eq!(question["type"], "choice");
        assert_eq!(question["criteria"]["option_0"], "Eres");
        assert_eq!(question["criteria"]["option_1"], "heiress");
        assert!(question["instructions"].as_str().unwrap().contains("blank"));
        assert_eq!(question["criteria"].as_object().unwrap().len(), 2);
    }

    #[test]
    fn probabilities_come_from_the_documented_answer() {
        let body = json!({
            "model": "jev-latest",
            "answers": {"word": {"type": "choice", "choice": "option_0", "confidence": 0.78,
                "probabilities": {"option_0": 0.85, "option_1": 0.15}}},
            "usage": {}
        });
        assert_eq!(parse_probabilities(&body, 2), Some(vec![0.85, 0.15]));
    }

    #[test]
    fn a_missing_field_gives_no_probabilities() {
        assert_eq!(parse_probabilities(&json!({}), 2), None);
        assert_eq!(parse_probabilities(&json!({"answers": {}}), 2), None);
        let one = json!({"answers": {"word": {"probabilities": {"option_0": 0.9}}}});
        assert_eq!(parse_probabilities(&one, 2), None);
        let text =
            json!({"answers": {"word": {"probabilities": {"option_0": "x", "option_1": 1}}}});
        assert_eq!(parse_probabilities(&text, 2), None);
    }

    #[test]
    fn an_empty_key_builds_no_decider() {
        assert!(JevDecider::new("").is_none());
        assert!(JevDecider::new("   ").is_none());
        assert!(JevDecider::new("jev-key").is_some());
    }

    /// Serves one HTTP request on a free port. Returns the URL and a handle that
    /// gives back the request text.
    fn serve_once(status: &str, body: &str, delay: Duration) -> (String, JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/systemone", listener.local_addr().unwrap());
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 4096];
            // Read the headers, then the body they announce.
            loop {
                let n = stream.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&request).to_string();
                if let Some(split) = text.find("\r\n\r\n") {
                    let length = text
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if request.len() >= split + 4 + length {
                        break;
                    }
                }
            }
            std::thread::sleep(delay);
            let _ = stream.write_all(response.as_bytes());
            String::from_utf8_lossy(&request).to_string()
        });
        (url, handle)
    }

    const ANSWER: &str = r#"{"answers":{"word":{"type":"choice","choice":"option_0","confidence":0.78,"probabilities":{"option_0":0.85,"option_1":0.15}}}}"#;

    #[test]
    fn choose_sends_the_key_and_the_sentence_and_reads_the_answer() {
        let (url, server) = serve_once("200 OK", ANSWER, Duration::ZERO);
        let decider = JevDecider::with_config("jev-key", &url, Duration::from_secs(2)).unwrap();

        let result = decider.choose("push the fix to ___", &options());

        assert_eq!(result, Some(vec![0.85, 0.15]));
        let request = server.join().unwrap();
        assert!(request
            .to_lowercase()
            .contains("authorization: bearer jev-key"));
        assert!(request.contains("push the fix to ___"));
        assert!(request.contains("option_1"));
    }

    #[test]
    fn choose_gives_none_for_an_error_status() {
        for status in [
            "401 Unauthorized",
            "422 Unprocessable Entity",
            "429 Too Many Requests",
            "529 Overloaded",
        ] {
            let (url, server) = serve_once(status, "{}", Duration::ZERO);
            let decider = JevDecider::with_config("k", &url, Duration::from_secs(2)).unwrap();
            assert_eq!(decider.choose("a ___ b", &options()), None, "{status}");
            server.join().unwrap();
        }
    }

    #[test]
    fn choose_gives_none_for_an_answer_without_probabilities() {
        let (url, server) = serve_once("200 OK", "{\"answers\":{}}", Duration::ZERO);
        let decider = JevDecider::with_config("k", &url, Duration::from_secs(2)).unwrap();
        assert_eq!(decider.choose("a ___ b", &options()), None);
        server.join().unwrap();
    }

    #[test]
    fn choose_gives_none_when_jev_is_too_slow() {
        let (url, server) = serve_once("200 OK", ANSWER, Duration::from_millis(800));
        let decider = JevDecider::with_config("k", &url, Duration::from_millis(150)).unwrap();
        let started = std::time::Instant::now();
        assert_eq!(decider.choose("a ___ b", &options()), None);
        assert!(started.elapsed() < Duration::from_millis(700));
        let _ = server.join();
    }

    #[test]
    fn choose_gives_none_when_nothing_listens() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/systemone", listener.local_addr().unwrap());
        drop(listener);
        let decider = JevDecider::with_config("k", &url, Duration::from_secs(1)).unwrap();
        assert_eq!(decider.choose("a ___ b", &options()), None);
    }

    #[test]
    fn choose_needs_two_options() {
        let decider =
            JevDecider::with_config("k", "http://127.0.0.1:9/", Duration::from_secs(1)).unwrap();
        assert_eq!(decider.choose("a ___ b", &["x".to_string()]), None);
    }
}
