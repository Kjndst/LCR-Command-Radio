use serde_json::{json, Value};
#[cfg(any(windows, test))]
use std::fmt;

pub fn pair_request(code: &str) -> Value {
    json!({"code": code})
}

pub fn pair_response(value: &Value) -> Option<(String, u64)> {
    Some((
        value.get("token")?.as_str()?.to_owned(),
        value
            .get("heartbeat_hint_ms")?
            .as_u64()
            .filter(|value| *value > 0)?,
    ))
}

#[cfg(any(windows, test))]
pub(crate) enum PairReply {
    Accepted {
        token: String,
        heartbeat_hint_ms: u64,
    },
    Denied,
    Transient,
}

#[cfg(any(windows, test))]
impl fmt::Debug for PairReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Accepted {
                heartbeat_hint_ms, ..
            } => f
                .debug_struct("Accepted")
                .field("paired", &true)
                .field("heartbeat_hint_ms", heartbeat_hint_ms)
                .finish(),
            Self::Denied => f.write_str("Denied"),
            Self::Transient => f.write_str("Transient"),
        }
    }
}

#[cfg(any(windows, test))]
impl PartialEq for PairReply {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Accepted {
                    token: left_token,
                    heartbeat_hint_ms: left_hint,
                },
                Self::Accepted {
                    token: right_token,
                    heartbeat_hint_ms: right_hint,
                },
            ) => left_token == right_token && left_hint == right_hint,
            (Self::Denied, Self::Denied) | (Self::Transient, Self::Transient) => true,
            _ => false,
        }
    }
}
#[cfg(any(windows, test))]
impl Eq for PairReply {}

#[cfg(any(windows, test))]
pub(crate) fn classify_pair(status: u16, body: &[u8]) -> PairReply {
    match status {
        200 => serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|value| pair_response(&value))
            .map(|(token, heartbeat_hint_ms)| PairReply::Accepted {
                token,
                heartbeat_hint_ms,
            })
            .unwrap_or(PairReply::Transient),
        401 => PairReply::Denied,
        _ => PairReply::Transient,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelayReceipt {
    pub(crate) cycle_generation: u64,
    pub(crate) command: bool,
    pub(crate) units_mask: u8,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ControlReply {
    pub(crate) generation: u64,
    pub(crate) relay: Option<RelayReceipt>,
}

pub fn state_request(desired: bool, generation: u64) -> Value {
    json!({
        "desiredTransmit": desired,
        "generation": generation
    })
}

pub fn heartbeat_request(generation: u64) -> Value {
    json!({
        "generation": generation
    })
}

fn parse_relay(value: &Value) -> Option<Option<RelayReceipt>> {
    if value.is_null() {
        return Some(None);
    }

    let object = value.as_object()?;

    if object.len() != 3 {
        return None;
    }

    let cycle_generation = object.get("cycleGeneration")?.as_u64()?;
    let command = object.get("command")?.as_bool()?;
    let units_mask = u8::try_from(object.get("unitsMask")?.as_u64()?).ok()?;

    if cycle_generation == 0 || units_mask & !0x0f != 0 || (!command && units_mask == 0) {
        return None;
    }

    Some(Some(RelayReceipt {
        cycle_generation,
        command,
        units_mask,
    }))
}

pub(crate) fn parse_control_reply(
    body: &[u8],
    expected_generation: u64,
    expected_desired: Option<bool>,
) -> Option<ControlReply> {
    let value = serde_json::from_slice::<Value>(body).ok()?;
    let object = value.as_object()?;

    let expected_fields = if expected_desired.is_some() { 3 } else { 2 };

    if object.len() != expected_fields {
        return None;
    }

    if object.get("generation")?.as_u64()? != expected_generation {
        return None;
    }

    match expected_desired {
        Some(expected) => {
            if object.get("desiredTransmit")?.as_bool()? != expected {
                return None;
            }
        }
        None if object.contains_key("desiredTransmit") => return None,
        None => {}
    }

    let relay = parse_relay(object.get("relay")?)?;

    if let Some(receipt) = relay {
        let expected_cycle = match expected_desired {
            Some(false) => expected_generation.checked_sub(1)?,
            Some(true) | None => expected_generation,
        };

        if receipt.cycle_generation != expected_cycle {
            return None;
        }
    }

    Some(ControlReply {
        generation: expected_generation,
        relay,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_json() {
        assert_eq!(pair_request("A").to_string(), r#"{"code":"A"}"#);

        assert_eq!(
            state_request(true, 7),
            json!({
                "desiredTransmit": true,
                "generation": 7
            })
        );

        assert_eq!(
            heartbeat_request(7),
            json!({
                "generation": 7
            })
        );

        assert_eq!(
            pair_response(&json!({"token":"t","heartbeat_hint_ms":250})),
            Some(("t".into(), 250))
        );

        assert_eq!(pair_response(&json!({"heartbeat_hint_ms":250})), None);

        assert_eq!(
            pair_response(&json!({"token":"t","heartbeat_hint_ms":0})),
            None
        );
    }

    #[test]
    fn pairing_response_classification_is_fail_closed() {
        assert_eq!(
            classify_pair(200, br#"{"token":"fake-token","heartbeat_hint_ms":250}"#),
            PairReply::Accepted {
                token: "fake-token".into(),
                heartbeat_hint_ms: 250,
            }
        );

        assert_eq!(
            classify_pair(200, br#"{"token":"fake-token"}"#),
            PairReply::Transient
        );

        assert_eq!(classify_pair(401, br#"{}"#), PairReply::Denied);
        assert_eq!(classify_pair(500, br#"{}"#), PairReply::Transient);
    }

    #[test]
    fn control_v1_reply_is_strict_and_generation_fenced() {
        let open = br#"{
            "generation":7,
            "desiredTransmit":true,
            "relay":{"cycleGeneration":7,"command":false,"unitsMask":5}
        }"#;

        let parsed = parse_control_reply(open, 7, Some(true)).unwrap();

        assert_eq!(
            parsed.relay,
            Some(RelayReceipt {
                cycle_generation: 7,
                command: false,
                units_mask: 5,
            })
        );

        assert!(parse_control_reply(open, 6, Some(true)).is_none());
        assert!(parse_control_reply(open, 7, Some(false)).is_none());

        assert!(parse_control_reply(
            br#"{
                "generation":7,
                "desiredTransmit":true,
                "relay":{"cycleGeneration":6,"command":false,"unitsMask":5}
            }"#,
            7,
            Some(true)
        )
        .is_none());

        let close = br#"{
            "generation":8,
            "desiredTransmit":false,
            "relay":{"cycleGeneration":7,"command":true,"unitsMask":0}
        }"#;

        assert!(parse_control_reply(close, 8, Some(false)).is_some());

        assert!(parse_control_reply(
            br#"{
                "generation":8,
                "desiredTransmit":false,
                "relay":{"cycleGeneration":8,"command":true,"unitsMask":0}
            }"#,
            8,
            Some(false)
        )
        .is_none());

        let heartbeat = br#"{
            "generation":7,
            "relay":{"cycleGeneration":7,"command":true,"unitsMask":0}
        }"#;

        assert!(parse_control_reply(heartbeat, 7, None).is_some());

        assert!(parse_control_reply(
            br#"{
                "generation":7,
                "relay":{"cycleGeneration":6,"command":true,"unitsMask":0}
            }"#,
            7,
            None
        )
        .is_none());

        assert!(parse_control_reply(
            br#"{
                "generation":7,
                "desiredTransmit":true,
                "relay":null,
                "clientConfig":null
            }"#,
            7,
            Some(true)
        )
        .is_none());

        assert!(
            parse_control_reply(br#"{"generation":7,"relay":null,"extra":1}"#, 7, None).is_none()
        );
    }

    #[test]
    fn accepted_pair_debug_never_contains_token() {
        let reply = PairReply::Accepted {
            token: "must-not-appear".into(),
            heartbeat_hint_ms: 250,
        };

        let debug = format!("{reply:?}");

        assert!(!debug.contains("must-not-appear"));
        assert!(debug.contains("heartbeat_hint_ms"));
    }
}
