#![allow(missing_docs)]

#[cfg(feature = "lcr-raw-opus-source")]
use crate::driver::raw_opus::RawOpusContext;

#[cfg(feature = "receive")]
use super::UdpRxMessage;
use super::{Interconnect, TrackContext, WsMessage};

use crate::{
    driver::{crypto::Cipher, Bitrate, Config, CryptoState},
    input::{AudioStreamError, Compose, Parsed},
};
use flume::Sender;
use std::{
    net::UdpSocket,
    sync::{atomic::AtomicU16, Arc, RwLock},
};
use symphonia_core::{errors::Error as SymphoniaError, formats::SeekedTo};

pub struct MixerConnection {
    pub cipher: Cipher,
    pub crypto_state: CryptoState,
    pub dave_session: Arc<RwLock<Option<davey::DaveSession>>>,
    pub dave_protocol_version: Arc<AtomicU16>,
    #[cfg(feature = "receive")]
    pub udp_rx: Sender<UdpRxMessage>,
    pub udp_tx: UdpSocket,
}

pub enum MixerMessage {
    AddTrack(Box<TrackContext>),
    SetTrack(Option<Box<TrackContext>>),
    #[cfg(feature = "lcr-raw-opus-source")]
    SetRawOpusSource(Option<Box<RawOpusContext>>),

    SetBitrate(Bitrate),
    SetConfig(Config),
    SetMute(bool),

    SetConn(MixerConnection, u32),
    Ws(Option<Sender<WsMessage>>),
    DropConn,

    ReplaceInterconnect(Interconnect),
    RebuildEncoder,

    Poison,
}

impl MixerMessage {
    #[must_use]
    pub fn is_mixer_maybe_live(&self) -> bool {
        match self {
            Self::AddTrack(_) | Self::SetTrack(Some(_)) | Self::SetConn(..) => true,
            #[cfg(feature = "lcr-raw-opus-source")]
            Self::SetRawOpusSource(Some(_)) => true,
            _ => false,
        }
    }
}

pub enum MixerInputResultMessage {
    CreateErr(Arc<AudioStreamError>),
    ParseErr(Arc<SymphoniaError>),
    Seek(
        Parsed,
        Option<Box<dyn Compose>>,
        Result<SeekedTo, Arc<SymphoniaError>>,
    ),
    Built(Parsed, Option<Box<dyn Compose>>),
}
