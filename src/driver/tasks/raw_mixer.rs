use super::{
    error::{Error, Result},
    message::*,
};
use crate::{
    constants::{SILENT_FRAME, UDP_KEEPALIVE_GAP, VOICE_PACKET_MAX},
    driver::{
        raw_opus::{RawOpusContext, RawOpusRead},
        CryptoMode,
    },
    Config,
};
use discortp::{
    discord::MutableKeepalivePacket,
    rtp::{MutableRtpPacket, RtpPacket},
    MutablePacket,
};
use flume::{Receiver, SendError, Sender};
use rand::random;
use std::{
    result::Result as StdResult,
    sync::{atomic::Ordering, Arc},
    time::Instant,
};
use tokio::runtime::Handle;

/// LCR raw-media transport state.
///
/// This profile intentionally has no generic tracks, decoder/mixer buffers,
/// resampler, PCM encoder, soft clipper, or blocking media pool. The existing
/// Songbird scheduler remains the sole packet clock and RTP counter owner.
pub struct Mixer {
    pub config: Arc<Config>,
    pub conn_active: Option<MixerConnection>,
    pub deadline: Instant,
    pub interconnect: Interconnect,
    pub mix_rx: Receiver<MixerMessage>,
    pub muted: bool,
    pub silence_frames: u8,
    pub ws: Option<Sender<WsMessage>>,

    pub keepalive_deadline: Instant,
    pub keepalive_packet: [u8; MutableKeepalivePacket::minimum_packet_size()],

    raw_opus_source: Option<Box<RawOpusContext>>,
}

impl Mixer {
    #[must_use]
    pub fn new(
        mix_rx: Receiver<MixerMessage>,
        _async_handle: Handle,
        interconnect: Interconnect,
        config: Config,
    ) -> Self {
        let deadline = Instant::now();

        Self {
            config: Arc::new(config),
            conn_active: None,
            deadline,
            interconnect,
            mix_rx,
            muted: false,
            silence_frames: 0,
            ws: None,
            keepalive_deadline: deadline,
            keepalive_packet: [0; MutableKeepalivePacket::minimum_packet_size()],
            raw_opus_source: None,
        }
    }

    pub(crate) fn do_rebuilds(
        &mut self,
        event_failure: bool,
        conn_failure: bool,
    ) -> StdResult<(), SendError<CoreMessage>> {
        if event_failure {
            self.rebuild_interconnect()?;
        }
        if conn_failure {
            self.full_reconnect_gateway()?;
        }
        Ok(())
    }

    pub(crate) fn rebuild_interconnect(&mut self) -> StdResult<(), SendError<CoreMessage>> {
        self.interconnect.core.send(CoreMessage::RebuildInterconnect)
    }

    pub(crate) fn full_reconnect_gateway(&mut self) -> StdResult<(), SendError<CoreMessage>> {
        self.conn_active = None;
        self.interconnect.core.send(CoreMessage::FullReconnect)
    }

    #[inline]
    pub(crate) fn handle_message(
        &mut self,
        msg: MixerMessage,
        packet: &mut [u8],
    ) -> (bool, bool, bool) {
        let events_failure = false;
        let mut conn_failure = false;
        let mut should_exit = false;

        match msg {
            MixerMessage::SetRawOpusSource(source) => {
                self.clear_raw_opus_source();
                self.raw_opus_source = source;
            },
            MixerMessage::SetBitrate(_) | MixerMessage::RebuildEncoder => {},
            MixerMessage::SetConfig(new_config) => {
                self.config = Arc::new(new_config);
            },
            MixerMessage::SetMute(muted) => {
                self.muted = muted;
            },
            MixerMessage::SetConn(conn, ssrc) => {
                self.conn_active = Some(conn);
                let mut rtp = MutableRtpPacket::new(packet).expect(
                    "Too few bytes in self.packet for RTP header. (Blame: VOICE_PACKET_MAX?)",
                );
                rtp.set_ssrc(ssrc);
                rtp.set_sequence(random::<u16>().into());
                rtp.set_timestamp(random::<u32>().into());
                self.deadline = Instant::now();
                self.update_keepalive(ssrc);
            },
            MixerMessage::DropConn => {
                self.conn_active = None;
            },
            MixerMessage::ReplaceInterconnect(interconnect) => {
                if let Some(ws) = &self.ws {
                    conn_failure |= ws
                        .send(WsMessage::ReplaceInterconnect(interconnect.clone()))
                        .is_err();
                }

                #[cfg(feature = "receive")]
                if let Some(conn) = &self.conn_active {
                    conn_failure |= conn
                        .udp_rx
                        .send(UdpRxMessage::ReplaceInterconnect(interconnect.clone()))
                        .is_err();
                }

                self.interconnect = interconnect;
            },
            MixerMessage::Ws(new_ws_handle) => {
                self.ws = new_ws_handle;
                if let Err(error) = self.send_gateway_speaking() {
                    conn_failure |= error.should_trigger_connect();
                }
            },
            MixerMessage::Poison => {
                should_exit = true;
            },
        }

        (events_failure, conn_failure, should_exit)
    }

    #[inline]
    fn clear_raw_opus_source(&mut self) {
        if let Some(source) = self.raw_opus_source.take() {
            source.mark_ended();
        }
    }

    #[inline]
    pub(crate) fn has_live_media_source(&self) -> bool {
        self.raw_opus_source.is_some()
    }

    #[inline]
    pub(crate) fn update_keepalive(&mut self, ssrc: u32) {
        let mut keepalive = MutableKeepalivePacket::new(&mut self.keepalive_packet[..])
            .expect("keepalive packet storage is exactly sized");
        keepalive.set_ssrc(ssrc);
        self.keepalive_deadline = self.deadline + UDP_KEEPALIVE_GAP;
    }

    #[inline]
    pub fn crypto_mode(&self) -> CryptoMode {
        self.conn_active
            .as_ref()
            .expect("raw media requires an active connection before packet generation")
            .crypto_state
            .kind()
    }

    /// Pull at most one encoded frame, or advance the explicit terminal-silence
    /// tail after the exact raw source ends/stops/errors.
    #[inline]
    pub fn mix_and_build_packet(&mut self, packet: &mut [u8]) -> Result<usize> {
        let crypto_mode = self.crypto_mode();
        let mut frame_len = None;

        if let Some(mut source) = self.raw_opus_source.take() {
            let mut rtp = MutableRtpPacket::new(packet).expect(
                "FATAL: Too few bytes in self.packet for RTP header. (Blame: VOICE_PACKET_MAX?)",
            );
            let payload = rtp.payload_mut();
            let first_payload_byte = crypto_mode.payload_prefix_len();
            let total_payload_space = payload.len().saturating_sub(crypto_mode.payload_suffix_len());

            if source.stop_requested() || first_payload_byte >= total_payload_space {
                source.mark_ended();
            } else {
                let slot = &mut payload[first_payload_byte..total_payload_space];
                match source.source.pull_opus(slot) {
                    RawOpusRead::Frame(len) if len > 0 && len <= slot.len() => {
                        self.raw_opus_source = Some(source);
                        frame_len = Some(len);
                    },
                    RawOpusRead::Frame(_) | RawOpusRead::End | RawOpusRead::Error => {
                        source.mark_ended();
                    },
                }
            }
        }

        if self.muted {
            frame_len = None;
        }

        if let Some(len) = frame_len {
            self.silence_frames = 5;
            return self.prep_passthrough_packet(len, packet);
        }

        if self.silence_frames == 0 {
            return Ok(0);
        }

        self.silence_frames -= 1;
        let mut rtp = MutableRtpPacket::new(packet).expect(
            "FATAL: Too few bytes in self.packet for RTP header. (Blame: VOICE_PACKET_MAX?)",
        );
        let payload = rtp.payload_mut();
        let prefix = crypto_mode.payload_prefix_len();
        let tail = crypto_mode.payload_suffix_len();
        let available_end = payload.len().saturating_sub(tail);
        let silence_end = prefix + SILENT_FRAME.len();
        if silence_end > available_end {
            return Ok(0);
        }
        payload[prefix..silence_end].copy_from_slice(&SILENT_FRAME[..]);
        self.prep_passthrough_packet(SILENT_FRAME.len(), packet)
    }

    #[inline]
    fn prep_passthrough_packet(&mut self, opus_len: usize, packet: &mut [u8]) -> Result<usize> {
        let conn = self
            .conn_active
            .as_mut()
            .expect("raw media packet generation requires an active connection");
        let mut rtp = MutableRtpPacket::new(packet).expect(
            "FATAL: Too few bytes in self.packet for RTP header. (Blame: VOICE_PACKET_MAX?)",
        );
        let payload = rtp.payload_mut();
        let crypto_mode = conn.crypto_state.kind();
        let first_payload_byte = crypto_mode.payload_prefix_len();
        let total_payload_space = payload.len() - crypto_mode.payload_suffix_len();
        if opus_len > total_payload_space.saturating_sub(first_payload_byte) {
            return Ok(0);
        }

        let mut payload_len = opus_len;
        if conn.dave_protocol_version.load(Ordering::Relaxed) != 0 {
            if let Some(ref mut dave_session) = *conn.dave_session.write().unwrap() {
                if dave_session.is_ready() {
                    let encrypted = dave_session
                        .encrypt_opus(&payload[first_payload_byte..first_payload_byte + payload_len])?
                        .into_owned();
                    payload_len = encrypted.len();
                    if payload_len > total_payload_space.saturating_sub(first_payload_byte) {
                        return Ok(0);
                    }
                    payload[first_payload_byte..first_payload_byte + payload_len]
                        .copy_from_slice(&encrypted);
                }
            }
        }

        let final_payload_size = conn
            .crypto_state
            .write_packet_nonce(&mut rtp, first_payload_byte + payload_len);
        conn.cipher
            .encrypt_pkt_in_place(&mut rtp, final_payload_size)?;

        Ok(RtpPacket::minimum_packet_size() + final_payload_size)
    }

    #[inline]
    pub(crate) fn send_packet(&self, packet: &[u8]) -> Result<()> {
        let conn = self
            .conn_active
            .as_ref()
            .expect("raw media packet send requires an active connection");
        conn.udp_tx
            .send(packet)
            .map(|_| ())
            .map_err(Error::from)
            .or_else(Error::disarm_would_block)
    }

    #[inline]
    pub(crate) fn check_and_send_keepalive(&mut self, now: Option<Instant>) -> Result<()> {
        if let Some(conn) = self.conn_active.as_mut() {
            let now = now.unwrap_or_else(Instant::now);
            if now >= self.keepalive_deadline {
                conn.udp_tx.send(&self.keepalive_packet)?;
                self.keepalive_deadline += UDP_KEEPALIVE_GAP;
            }
        }
        Ok(())
    }

    #[inline]
    pub(crate) fn send_gateway_speaking(&self) -> Result<()> {
        if let Some(ws) = &self.ws {
            ws.send(WsMessage::Speaking(true))?;
        }
        Ok(())
    }

    #[inline]
    pub(crate) fn send_gateway_not_speaking(&self) {
        if let Some(ws) = &self.ws {
            drop(ws.send(WsMessage::Speaking(false)));
        }
    }


}

#[cfg(test)]
pub(crate) fn lcr_test_mixer_with_raw_source<S>(
    source: S,
) -> (Mixer, crate::driver::RawOpusHandle, flume::Receiver<CoreMessage>)
where
    S: crate::driver::RawOpusSource,
{
    use crate::driver::{LcrCausalDispatcher, LcrImmediateDispatcher};
    use std::{
        net::UdpSocket,
        sync::{atomic::AtomicU16, RwLock},
    };

    let (mix_tx, mix_rx) = flume::unbounded();
    let (core_tx, core_rx) = flume::unbounded();
    let (event_tx, _event_rx) = flume::unbounded();
    let (udp_rx, _udp_rx_receiver) = flume::unbounded();
    let interconnect = Interconnect {
        core: core_tx,
        events: event_tx,
        mixer: mix_tx,
        lcr_causal: LcrCausalDispatcher::new().0,
        lcr_immediate: LcrImmediateDispatcher::default(),
    };
    let mut mixer = Mixer::new(
        mix_rx,
        Handle::current(),
        interconnect,
        Config::default(),
    );
    let udp_tx = UdpSocket::bind("127.0.0.1:0").expect("bind raw-media test UDP socket");
    udp_tx
        .connect("127.0.0.1:9")
        .expect("connect raw-media test UDP socket");
    let mode = CryptoMode::Aes256Gcm;
    let connection = MixerConnection {
        cipher: mode.cipher_from_key(&[0_u8; 32]).expect("static AES key"),
        crypto_state: mode.into(),
        dave_session: Arc::new(RwLock::new(None)),
        dave_protocol_version: Arc::new(AtomicU16::new(0)),
        udp_rx,
        udp_tx,
    };
    let mut packet = [0_u8; VOICE_PACKET_MAX];
    assert_eq!(
        mixer.handle_message(MixerMessage::SetConn(connection, 42), &mut packet),
        (false, false, false)
    );
    let (handle, context) = RawOpusContext::new(source);
    assert_eq!(
        mixer.handle_message(
            MixerMessage::SetRawOpusSource(Some(Box::new(context))),
            &mut packet,
        ),
        (false, false, false)
    );
    (mixer, handle, core_rx)
}
const _: usize = VOICE_PACKET_MAX;
#[cfg(test)]
mod lcr_raw_media_tests {
    use super::*;
    use crate::driver::{
        LcrCausalDispatcher, LcrImmediateDispatcher, RawOpusHandle, RawOpusSource,
    };
    use std::{
        collections::VecDeque,
        net::UdpSocket,
        sync::{
            atomic::{AtomicU16, AtomicUsize, Ordering as AtomicOrdering},
            Arc, RwLock,
        },
    };

    const FRAME_A: &[u8] = &[0x11, 0x22, 0x33, 0x44];
    const FRAME_B: &[u8] = &[0xaa, 0xbb, 0xcc, 0xdd, 0xee];

    enum ScriptedRead {
        Frame(&'static [u8]),
        End,
        Error,
    }

    struct ScriptedRawSource {
        reads: VecDeque<ScriptedRead>,
        pulls: Arc<AtomicUsize>,
    }

    impl RawOpusSource for ScriptedRawSource {
        fn pull_opus(&mut self, dst: &mut [u8]) -> RawOpusRead {
            self.pulls.fetch_add(1, AtomicOrdering::SeqCst);
            match self.reads.pop_front().unwrap_or(ScriptedRead::End) {
                ScriptedRead::Frame(frame) => {
                    dst[..frame.len()].copy_from_slice(frame);
                    RawOpusRead::Frame(frame.len())
                },
                ScriptedRead::End => RawOpusRead::End,
                ScriptedRead::Error => RawOpusRead::Error,
            }
        }
    }

    fn scripted_source(reads: Vec<ScriptedRead>) -> (ScriptedRawSource, Arc<AtomicUsize>) {
        let pulls = Arc::new(AtomicUsize::new(0));
        (
            ScriptedRawSource {
                reads: VecDeque::from(reads),
                pulls: Arc::clone(&pulls),
            },
            pulls,
        )
    }

    fn test_connection() -> MixerConnection {
        let udp_tx = UdpSocket::bind("127.0.0.1:0").expect("bind raw-media test UDP socket");
        udp_tx
            .connect("127.0.0.1:9")
            .expect("connect raw-media test UDP socket");
        let (udp_rx, _unused) = flume::unbounded();
        let mode = CryptoMode::Aes256Gcm;
        MixerConnection {
            cipher: mode.cipher_from_key(&[0u8; 32]).expect("static AES key"),
            crypto_state: mode.into(),
            dave_session: Arc::new(RwLock::new(None)),
            dave_protocol_version: Arc::new(AtomicU16::new(0)),
            udp_rx,
            udp_tx,
        }
    }

    fn test_mixer() -> Mixer {
        let (mix_tx, mix_rx) = flume::unbounded();
        let (core_tx, _core_rx) = flume::unbounded();
        let (event_tx, _event_rx) = flume::unbounded();
        let interconnect = Interconnect {
            core: core_tx,
            events: event_tx,
            mixer: mix_tx,
            lcr_causal: LcrCausalDispatcher::new().0,
            lcr_immediate: LcrImmediateDispatcher::default(),
        };
        let mut mixer = Mixer::new(mix_rx, Handle::current(), interconnect, Config::default());
        let mut packet = [0u8; VOICE_PACKET_MAX];
        assert_eq!(
            mixer.handle_message(MixerMessage::SetConn(test_connection(), 42), &mut packet),
            (false, false, false)
        );
        mixer
    }

    fn install_raw(mixer: &mut Mixer, source: ScriptedRawSource) -> RawOpusHandle {
        let (handle, context) = RawOpusContext::new(source);
        let mut packet = [0u8; VOICE_PACKET_MAX];
        assert_eq!(
            mixer.handle_message(
                MixerMessage::SetRawOpusSource(Some(Box::new(context))),
                &mut packet,
            ),
            (false, false, false)
        );
        handle
    }

    #[tokio::test]
    async fn raw_media_pulls_at_most_one_source_frame_per_tick() {
        let mut mixer = test_mixer();
        let (source, pulls) = scripted_source(vec![
            ScriptedRead::Frame(FRAME_A),
            ScriptedRead::Frame(FRAME_B),
        ]);
        let _handle = install_raw(&mut mixer, source);
        let mut packet = [0u8; VOICE_PACKET_MAX];

        assert!(mixer.mix_and_build_packet(&mut packet).unwrap() > 0);
        assert_eq!(pulls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(mixer.silence_frames, 5);

        assert!(mixer.mix_and_build_packet(&mut packet).unwrap() > 0);
        assert_eq!(pulls.load(AtomicOrdering::SeqCst), 2);
        assert_eq!(mixer.silence_frames, 5);
    }

    #[tokio::test]
    async fn raw_media_end_and_error_emit_exact_five_frame_tail() {
        for terminal in [ScriptedRead::End, ScriptedRead::Error] {
            let mut mixer = test_mixer();
            let (source, pulls) = scripted_source(vec![ScriptedRead::Frame(FRAME_A), terminal]);
            let handle = install_raw(&mut mixer, source);
            let mut packet = [0u8; VOICE_PACKET_MAX];

            assert!(mixer.mix_and_build_packet(&mut packet).unwrap() > 0);
            assert_eq!(mixer.silence_frames, 5);

            for expected_remaining in [4u8, 3, 2, 1, 0] {
                assert!(mixer.mix_and_build_packet(&mut packet).unwrap() > 0);
                assert_eq!(mixer.silence_frames, expected_remaining);
            }

            assert!(handle.is_ended());
            assert_eq!(pulls.load(AtomicOrdering::SeqCst), 2);
            assert_eq!(mixer.mix_and_build_packet(&mut packet).unwrap(), 0);
            assert_eq!(mixer.silence_frames, 0);
            assert_eq!(pulls.load(AtomicOrdering::SeqCst), 2);
            assert!(!mixer.has_live_media_source());
        }
    }

    #[tokio::test]
    async fn raw_media_stop_enters_tail_without_extra_pull() {
        let mut mixer = test_mixer();
        let (source, pulls) = scripted_source(vec![
            ScriptedRead::Frame(FRAME_A),
            ScriptedRead::Frame(FRAME_B),
        ]);
        let handle = install_raw(&mut mixer, source);
        let mut packet = [0u8; VOICE_PACKET_MAX];

        assert!(mixer.mix_and_build_packet(&mut packet).unwrap() > 0);
        assert_eq!(pulls.load(AtomicOrdering::SeqCst), 1);
        handle.stop();

        for expected_remaining in [4u8, 3, 2, 1, 0] {
            assert!(mixer.mix_and_build_packet(&mut packet).unwrap() > 0);
            assert_eq!(mixer.silence_frames, expected_remaining);
        }

        assert!(handle.is_ended());
        assert_eq!(pulls.load(AtomicOrdering::SeqCst), 1);
        assert_eq!(mixer.mix_and_build_packet(&mut packet).unwrap(), 0);
    }

    #[tokio::test]
    async fn raw_media_stale_handle_cannot_stop_replacement() {
        let mut mixer = test_mixer();
        let (source_a, pulls_a) = scripted_source(vec![ScriptedRead::Frame(FRAME_A)]);
        let old_handle = install_raw(&mut mixer, source_a);
        let (source_b, pulls_b) = scripted_source(vec![ScriptedRead::Frame(FRAME_B)]);
        let new_handle = install_raw(&mut mixer, source_b);

        assert!(old_handle.is_ended());
        assert!(!new_handle.is_ended());
        old_handle.stop();

        let mut packet = [0u8; VOICE_PACKET_MAX];
        assert!(mixer.mix_and_build_packet(&mut packet).unwrap() > 0);
        assert_eq!(pulls_a.load(AtomicOrdering::SeqCst), 0);
        assert_eq!(pulls_b.load(AtomicOrdering::SeqCst), 1);
        assert!(!new_handle.is_ended());
    }

    #[tokio::test]
    async fn raw_media_dropconn_setconn_preserves_source_instance() {
        let mut mixer = test_mixer();
        let (source, pulls) = scripted_source(vec![ScriptedRead::Frame(FRAME_A)]);
        let handle = install_raw(&mut mixer, source);
        let mut packet = [0u8; VOICE_PACKET_MAX];

        assert_eq!(
            mixer.handle_message(MixerMessage::DropConn, &mut packet),
            (false, false, false)
        );
        assert!(mixer.conn_active.is_none());
        assert!(mixer.has_live_media_source());
        assert!(!handle.is_ended());

        assert_eq!(
            mixer.handle_message(MixerMessage::SetConn(test_connection(), 43), &mut packet),
            (false, false, false)
        );
        assert!(mixer.has_live_media_source());
        assert!(!handle.is_ended());
        assert!(mixer.mix_and_build_packet(&mut packet).unwrap() > 0);
        assert_eq!(pulls.load(AtomicOrdering::SeqCst), 1);
    }
}