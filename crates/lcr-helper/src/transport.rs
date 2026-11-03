//! Response classification shared by the native WinHTTP adapter and deterministic tests.

use crate::{protocol::ControlReply, reconciler::Desired};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ControlOutcome {
    Idle,
    StateAck {
        desired: Desired,
        generation: u64,
        reply: ControlReply,
    },
    Heartbeat(ControlReply),
    Unauthorized,
    Transient,
}

pub fn classify(status: u16, body: &[u8], desired: Desired, generation: u64) -> ControlOutcome {
    if status == 401 {
        return ControlOutcome::Unauthorized;
    }

    if status != 202 {
        return ControlOutcome::Transient;
    }

    let expected = matches!(desired, Desired::Open);

    match crate::protocol::parse_control_reply(body, generation, Some(expected)) {
        Some(reply) => ControlOutcome::StateAck {
            desired,
            generation,
            reply,
        },
        None => ControlOutcome::Transient,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CredentialProbe {
    Active,
    Rejected,
    Transient,
}

pub fn classify_credential_probe(status: u16, body: &[u8]) -> CredentialProbe {
    match status {
        401 => CredentialProbe::Rejected,
        200 => {
            let active = serde_json::from_slice::<serde_json::Value>(body)
                .ok()
                .is_some_and(|value| {
                    value.as_object().is_some_and(|map| {
                        map.len() == 1
                            && map.get("active").and_then(|value| value.as_bool()) == Some(true)
                    })
                });
            if active {
                CredentialProbe::Active
            } else {
                CredentialProbe::Transient
            }
        }
        _ => CredentialProbe::Transient,
    }
}

pub fn classify_heartbeat(status: u16, body: &[u8], generation: u64) -> ControlOutcome {
    if status == 401 {
        return ControlOutcome::Unauthorized;
    }

    if status != 200 {
        return ControlOutcome::Transient;
    }

    match crate::protocol::parse_control_reply(body, generation, None) {
        Some(reply) => ControlOutcome::Heartbeat(reply),
        None => ControlOutcome::Transient,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cold_restart_probe_never_mistakes_legacy_heartbeat_for_active_credential() {
        assert_eq!(
            classify_credential_probe(200, br#"{"active":true}"#),
            CredentialProbe::Active
        );
        assert_eq!(
            classify_credential_probe(401, br#"{}"#),
            CredentialProbe::Rejected
        );
        assert_eq!(
            classify_credential_probe(503, br#"{}"#),
            CredentialProbe::Transient
        );
        assert_eq!(
            classify_credential_probe(409, br#"{}"#),
            CredentialProbe::Transient
        );
        assert_eq!(
            classify_credential_probe(200, br#"{"generation":1}"#),
            CredentialProbe::Transient
        );
        assert_eq!(
            classify_credential_probe(200, br#"{"active":true,"extra":1}"#),
            CredentialProbe::Transient
        );
        assert_eq!(
            classify_credential_probe(200, br#"not-json"#),
            CredentialProbe::Transient
        );
    }

    #[test]
    fn state_and_heartbeat_outcomes_are_direct_and_fail_closed() {
        assert!(matches!(
            classify(
                202,
                br#"{
                    "desiredTransmit":true,
                    "generation":4,
                    "relay":{"cycleGeneration":4,"command":false,"unitsMask":1}
                }"#,
                Desired::Open,
                4
            ),
            ControlOutcome::StateAck {
                desired: Desired::Open,
                generation: 4,
                ..
            }
        ));

        assert_eq!(
            classify(
                202,
                br#"{
                    "desiredTransmit":true,
                    "generation":4,
                    "relay":{"cycleGeneration":3,"command":false,"unitsMask":1}
                }"#,
                Desired::Open,
                4
            ),
            ControlOutcome::Transient
        );

        assert!(matches!(
            classify(
                202,
                br#"{
                    "desiredTransmit":false,
                    "generation":5,
                    "relay":{"cycleGeneration":4,"command":true,"unitsMask":0}
                }"#,
                Desired::Closed,
                5
            ),
            ControlOutcome::StateAck {
                desired: Desired::Closed,
                generation: 5,
                ..
            }
        ));

        assert_eq!(
            classify(
                202,
                br#"{
                    "desiredTransmit":false,
                    "generation":5,
                    "relay":{"cycleGeneration":5,"command":true,"unitsMask":0}
                }"#,
                Desired::Closed,
                5
            ),
            ControlOutcome::Transient
        );

        assert_eq!(
            classify(
                202,
                br#"{
                    "desiredTransmit":true,
                    "generation":4,
                    "relay":null,
                    "clientConfig":null
                }"#,
                Desired::Open,
                4
            ),
            ControlOutcome::Transient
        );

        assert_eq!(
            classify(401, b"", Desired::Open, 4),
            ControlOutcome::Unauthorized
        );

        assert_eq!(
            classify(503, b"", Desired::Open, 4),
            ControlOutcome::Transient
        );

        assert!(matches!(
            classify_heartbeat(
                200,
                br#"{
                    "generation":4,
                    "relay":{"cycleGeneration":4,"command":true,"unitsMask":0}
                }"#,
                4
            ),
            ControlOutcome::Heartbeat(_)
        ));

        assert_eq!(
            classify_heartbeat(
                200,
                br#"{
                    "generation":4,
                    "relay":{"cycleGeneration":3,"command":true,"unitsMask":0}
                }"#,
                4
            ),
            ControlOutcome::Transient
        );

        assert_eq!(classify_heartbeat(204, b"", 4), ControlOutcome::Transient);
    }
}
