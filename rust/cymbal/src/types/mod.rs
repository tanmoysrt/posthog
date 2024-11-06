use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{digest::Update, Sha512};

use crate::frames::{Frame, RawFrame};

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Mechanism {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "type")]
    pub mechanism_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthetic: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Stacktrace {
    Raw { frames: Vec<RawFrame> },
    Resolved { frames: Vec<Frame> },
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Exception {
    #[serde(rename = "type")]
    pub exception_type: String,
    #[serde(rename = "value")]
    pub exception_message: String,
    pub mechanism: Option<Mechanism>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "stacktrace")]
    pub stack: Option<Stacktrace>,
}

// Given a Clickhouse Event's properties, we care about the contents
// of only a small subset. This struct is used to give us a strongly-typed
// "view" of those event properties we care about.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ErrProps {
    #[serde(rename = "$exception_list")]
    pub exception_list: Option<Vec<Exception>>, // Required from exception producers - we will not process events without this. Optional to support older clients, should eventually be removed
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "$exception_type")]
    pub exception_type: Option<String>, // legacy, overridden by exception_list
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "$exception_message")]
    pub exception_message: Option<String>, // legacy, overridden by exception_list
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "$exception_stack_trace_raw")]
    pub exception_stack_trace_raw: Option<String>, // Not all exceptions have a stack trace
    #[serde(rename = "$exception_level")]
    pub exception_level: Option<String>, // We generally don't touch this, but we break it out explicitly for users. Not all exceptions have a level
    #[serde(flatten)] // A catch-all for all the properties we don't "care" about
    pub other: HashMap<String, Value>,
}

impl Exception {
    pub fn include_in_fingerprint(&self, h: &mut Sha512) {
        h.update(self.exception_type.as_bytes());
        h.update(self.exception_message.as_bytes());
        let Some(Stacktrace::Resolved { frames }) = &self.stack else {
            return;
        };

        let has_no_resolved = !frames.iter().any(|f| f.resolved);
        let has_no_in_app = !frames.iter().any(|f| f.in_app);

        if has_no_in_app {
            // TODO: we should try to be smarter about handling the case when
            // there are no in-app frames
            if let Some(f) = frames.first() {
                f.include_in_fingerprint(h)
            }
            return;
        }

        for frame in frames {
            if (has_no_resolved || frame.resolved) && frame.in_app {
                frame.include_in_fingerprint(h)
            }
        }
    }
}

#[cfg(test)]
mod test {
    use common_types::ClickHouseEvent;
    use serde_json::Error;

    use crate::{frames::RawFrame, types::Stacktrace};

    use super::ErrProps;

    #[test]
    fn it_deserialises_error_props() {
        let raw: &'static str = include_str!("../../tests/static/raw_ch_exception_list.json");

        let raw: ClickHouseEvent = serde_json::from_str(raw).unwrap();

        let props: ErrProps = serde_json::from_str(&raw.properties.unwrap()).unwrap();
        let exception_list = &props.exception_list.unwrap();

        assert_eq!(exception_list.len(), 1);
        assert_eq!(
            exception_list[0].exception_type,
            "UnhandledRejection".to_string()
        );
        assert_eq!(
            exception_list[0].exception_message,
            "Unexpected usage".to_string()
        );
        let mechanism = exception_list[0].mechanism.as_ref().unwrap();
        assert_eq!(mechanism.handled, Some(false));
        assert_eq!(mechanism.mechanism_type, None);
        assert_eq!(mechanism.source, None);
        assert_eq!(mechanism.synthetic, Some(false));

        let Stacktrace::Raw { frames } = exception_list[0].stack.as_ref().unwrap() else {
            panic!("Expected a Raw stacktrace")
        };
        assert_eq!(frames.len(), 2);
        let RawFrame::JavaScript(frame) = &frames[0];

        assert_eq!(
            frame.source_url,
            Some("https://app-static.eu.posthog.com/static/chunk-PGUQKT6S.js".to_string())
        );
        assert_eq!(frame.fn_name, "?".to_string());
        assert!(frame.in_app);
        assert_eq!(frame.line, 64);
        assert_eq!(frame.column, 25112);

        let RawFrame::JavaScript(frame) = &frames[1];
        assert_eq!(
            frame.source_url,
            Some("https://app-static.eu.posthog.com/static/chunk-PGUQKT6S.js".to_string())
        );
        assert_eq!(frame.fn_name, "n.loadForeignModule".to_string());
        assert!(frame.in_app);
        assert_eq!(frame.line, 64);
        assert_eq!(frame.column, 15003);

        assert_eq!(props.exception_type, None);
        assert_eq!(props.exception_message, None);
        assert_eq!(props.exception_stack_trace_raw, None);
        assert_eq!(props.exception_level, Some("error".to_string()));
    }

    #[test]
    fn it_rejects_invalid_error_props() {
        let raw: &'static str = r#"{
            "$exception_list": []
        }"#;

        let props: Result<ErrProps, Error> = serde_json::from_str(raw);
        assert!(props.is_ok());
        assert_eq!(props.unwrap().exception_list.unwrap().len(), 0);

        let raw: &'static str = r#"{
            "$exception_list": [{
                "type": "UnhandledRejection"
            }]
        }"#;

        let props: Result<ErrProps, Error> = serde_json::from_str(raw);
        assert!(props.is_err());
        assert_eq!(
            props.unwrap_err().to_string(),
            "missing field `value` at line 4 column 13"
        );

        let raw: &'static str = r#"{
            "$exception_list": [{
                "typo": "UnhandledRejection",
                "value": "x"
            }]
        }"#;

        let props: Result<ErrProps, Error> = serde_json::from_str(raw);
        assert!(props.is_err());
        assert_eq!(
            props.unwrap_err().to_string(),
            "missing field `type` at line 5 column 13"
        );
    }
}
