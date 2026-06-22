use serde::Deserialize;
use serde_json::Value;

use crate::proto::ErrCode;

#[derive(Debug, Deserialize)]
pub struct Request {
    pub id: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

pub fn encode_response(id: &str, result: Value) -> String {
    serde_json::json!({"id": id, "result": result}).to_string() + "\n"
}

pub fn encode_error(id: &str, code: ErrCode, msg: &str) -> String {
    serde_json::json!({
        "id": id,
        "error": {
            "code": code.as_str(),
            "message": msg
        }
    })
    .to_string()
        + "\n"
}

pub fn encode_event(event: &str, data: Value) -> String {
    serde_json::json!({"event": event, "data": data}).to_string() + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_decodes() {
        let r: Request = serde_json::from_str(r#"{"id":"1","method":"list_hosts","params":{}}"#).unwrap();
        assert_eq!(r.method, "list_hosts");
        assert_eq!(r.id, "1");
    }

    #[test]
    fn request_defaults_empty_params() {
        let r: Request = serde_json::from_str(r#"{"id":"2","method":"ping"}"#).unwrap();
        assert!(r.params.is_object() || r.params.is_null());
    }

    #[test]
    fn encodings_are_newline_terminated() {
        let ok = encode_response("1", serde_json::json!({"ok":true}));
        assert!(ok.ends_with('\n') && ok.contains("\"result\""));
        let er = encode_error("1", crate::proto::ErrCode::BadParams, "nope");
        assert!(er.ends_with('\n') && er.contains("\"error\"") && er.contains("bad_params"));
        let ev = encode_event("tunnel_status_changed", serde_json::json!({"name":"x"}));
        assert!(ev.ends_with('\n') && ev.contains("\"event\""));
    }
}
