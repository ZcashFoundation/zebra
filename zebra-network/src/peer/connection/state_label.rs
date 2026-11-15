//! Allocation-free labels for the aggregate connection state gauge.
//!
//! Connections update their state label for every peer message, so labels are small `Copy` keys
//! built from `&'static str` parts, and the metric key for each distinct label is built once per
//! thread.

use std::{cell::RefCell, collections::HashMap, fmt};

use metrics::{Gauge, Key, Label, Level, Metadata};

use crate::peer::PeerError;

const STATE_GAUGE: &str = "zebra.net.connection.state";

/// The connection state that a connection is counted under in [`STATE_GAUGE`].
///
/// The label text is `{state}` or `{state}::{extra}`.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub(super) struct StateLabel {
    state: StateName,
    extra: Option<ExtraInfo>,
}

impl StateLabel {
    pub(super) fn new(state: StateName, extra: Option<ExtraInfo>) -> Self {
        Self { state, extra }
    }

    /// Counts a connection in this state.
    pub(super) fn increment(self) {
        self.with_gauge(|gauge| gauge.increment(1.0));
    }

    /// Stops counting a connection in this state.
    pub(super) fn decrement(self) {
        self.with_gauge(|gauge| gauge.decrement(1.0));
    }

    /// Runs `update` on the gauge for this label in the active recorder.
    ///
    /// The metric key is built once per thread and label, so updates don't format the label. The
    /// gauge is registered with the active recorder on every update, because a cached handle
    /// stays bound to the recorder that registered it, and a replaced recorder can't be detected.
    fn with_gauge(self, update: impl FnOnce(&Gauge)) {
        thread_local! {
            static KEYS: RefCell<HashMap<StateLabel, Key>> = RefCell::new(HashMap::new());
        }

        static METADATA: Metadata<'static> =
            Metadata::new(module_path!(), Level::INFO, Some(module_path!()));

        KEYS.with_borrow_mut(|keys| {
            let key = keys.entry(self).or_insert_with(|| {
                Key::from_parts(STATE_GAUGE, vec![Label::new("command", self.to_string())])
            });
            update(&metrics::with_recorder(|recorder| {
                recorder.register_gauge(key, &METADATA)
            }));
        });
    }
}

impl fmt::Display for StateLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.state)?;
        if let Some(extra) = self.extra {
            write!(f, "::{extra}")?;
        }
        Ok(())
    }
}

/// Prints the label text, so logs show the same value as the metric.
impl fmt::Debug for StateLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "\"{self}\"")
    }
}

/// The Zebra internal state of a connection.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub(super) enum StateName {
    AwaitingRequest,
    AwaitingResponse(HandlerName),
    Failed,
}

impl fmt::Display for StateName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StateName::AwaitingRequest => f.write_str("AwaitingRequest"),
            StateName::AwaitingResponse(handler) => write!(f, "AwaitingResponse({handler})"),
            StateName::Failed => f.write_str("Failed"),
        }
    }
}

/// The Zebra internal response handler type, without any handler contents.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub(super) enum HandlerName {
    /// A finished handler, with its response command or error kind.
    Finished(&'static str),
    /// A handler that is still waiting for peer messages.
    Pending(&'static str),
}

impl fmt::Display for HandlerName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HandlerName::Finished(outcome) => write!(f, "Finished({outcome})"),
            HandlerName::Pending(name) => f.write_str(name),
        }
    }
}

/// What a connection was doing when its state was last updated.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub(super) enum ExtraInfo {
    /// A peer message was received as a response to an outbound request.
    OutRsp(&'static str),
    /// An outbound request was sent to the peer.
    OutReq(&'static str),
    /// An outbound request was canceled before it was sent.
    OutReqCanceled(&'static str),
    /// A peer message was received.
    InMsg(&'static str),
    /// A peer message was mapped to an inbound request.
    InReq(&'static str),
    /// The inbound service returned an error for an inbound request.
    InReqRspError(&'static str),
    /// The inbound service returned a response.
    InRsp(&'static str),
    /// The inbound service was overloaded or timed out.
    InReqOverloaded {
        request: &'static str,
        overload: Overload,
        /// Whether the connection was closed, or the overload was ignored.
        closed: bool,
    },
}

impl fmt::Display for ExtraInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExtraInfo::OutRsp(command) => write!(f, "Out::Rsp::{command}"),
            ExtraInfo::OutReq(command) => write!(f, "Out::Req::{command}"),
            ExtraInfo::OutReqCanceled(command) => write!(f, "Out::Req::Canceled::{command}"),
            ExtraInfo::InMsg(command) => write!(f, "In::Msg::{command}"),
            ExtraInfo::InReq(command) => write!(f, "In::Req::{command}"),
            ExtraInfo::InReqRspError(command) => write!(f, "In::Req::{command}/Rsp::Error"),
            ExtraInfo::InRsp(command) => write!(f, "In::Rsp::{command}"),
            ExtraInfo::InReqOverloaded {
                request,
                overload,
                closed,
            } => {
                let outcome = if *closed { "Error" } else { "Ignored" };
                write!(
                    f,
                    "In::Req::{request}/Rsp::{}::{outcome}",
                    overload.message()
                )
            }
        }
    }
}

/// The ways the inbound service can fail to respond to an inbound request.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum Overload {
    /// The inbound service is over capacity.
    Overloaded,
    /// The inbound service did not respond in time.
    Timeout,
}

impl Overload {
    /// Returns the error that closes the connection, or is logged when the overload is ignored.
    pub(super) fn error(self) -> PeerError {
        match self {
            Overload::Overloaded => PeerError::Overloaded,
            Overload::Timeout => PeerError::InboundTimeout,
        }
    }

    /// Returns the display text of [`Self::error()`].
    fn message(self) -> &'static str {
        match self {
            Overload::Overloaded => "Internal services over capacity",
            Overload::Timeout => "Internal services timed out",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_keep_their_metric_values() {
        let label = |state, extra| StateLabel::new(state, extra).to_string();
        let awaiting_peers = StateName::AwaitingResponse(HandlerName::Pending("Peers"));

        assert_eq!(label(StateName::AwaitingRequest, None), "AwaitingRequest");
        assert_eq!(label(StateName::Failed, None), "Failed");
        assert_eq!(
            label(
                StateName::AwaitingResponse(HandlerName::Finished("Serialization")),
                None
            ),
            "AwaitingResponse(Finished(Serialization))"
        );
        assert_eq!(
            label(awaiting_peers, Some(ExtraInfo::OutRsp("addr"))),
            "AwaitingResponse(Peers)::Out::Rsp::addr"
        );
        assert_eq!(
            label(StateName::AwaitingRequest, Some(ExtraInfo::OutReq("Peers"))),
            "AwaitingRequest::Out::Req::Peers"
        );
        assert_eq!(
            label(
                StateName::AwaitingRequest,
                Some(ExtraInfo::OutReqCanceled("Peers"))
            ),
            "AwaitingRequest::Out::Req::Canceled::Peers"
        );
        assert_eq!(
            label(StateName::AwaitingRequest, Some(ExtraInfo::InMsg("ping"))),
            "AwaitingRequest::In::Msg::ping"
        );
        assert_eq!(
            label(StateName::AwaitingRequest, Some(ExtraInfo::InReq("Peers"))),
            "AwaitingRequest::In::Req::Peers"
        );
        assert_eq!(
            label(
                StateName::AwaitingRequest,
                Some(ExtraInfo::InReqRspError("Peers"))
            ),
            "AwaitingRequest::In::Req::Peers/Rsp::Error"
        );
        assert_eq!(
            label(StateName::AwaitingRequest, Some(ExtraInfo::InRsp("Peers"))),
            "AwaitingRequest::In::Rsp::Peers"
        );
        assert_eq!(
            label(
                StateName::AwaitingRequest,
                Some(ExtraInfo::InReqOverloaded {
                    request: "Peers",
                    overload: Overload::Overloaded,
                    closed: true,
                })
            ),
            "AwaitingRequest::In::Req::Peers/Rsp::Internal services over capacity::Error"
        );
        assert_eq!(
            label(
                StateName::AwaitingRequest,
                Some(ExtraInfo::InReqOverloaded {
                    request: "Peers",
                    overload: Overload::Timeout,
                    closed: false,
                })
            ),
            "AwaitingRequest::In::Req::Peers/Rsp::Internal services timed out::Ignored"
        );
    }

    #[test]
    fn overload_messages_match_their_errors() {
        for overload in [Overload::Overloaded, Overload::Timeout] {
            assert_eq!(overload.message(), overload.error().to_string());
        }
    }
}
