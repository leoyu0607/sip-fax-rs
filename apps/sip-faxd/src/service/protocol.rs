//! The control protocol: one JSON object per line, requests from the client, replies from the
//! service (spec `docs/design/m4-web.md` section 7.1).
//!
//! ```text
//! {"cmd":"status"}                          -> {"type":"status", ...}
//! {"cmd":"calls"}                           -> {"type":"calls","calls":[...]}
//! {"cmd":"reload"}                          -> {"type":"reload","message":"..."} | {"type":"error",...}
//! {"cmd":"stop","force":false,"timeout":60} -> {"type":"stopping",...} {"type":"waiting",...}* {"type":"stopped"}
//! {"cmd":"subscribe"}                       -> {"type":"subscribed","calls":[...]} then {"type":"event",...}*
//! ```

use serde::{Deserialize, Serialize};

/// A request line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Request {
    /// The service's state.
    Status,
    /// The calls in progress.
    Calls,
    /// Load the runtime settings from the database again.
    Reload,
    /// Stop: no new calls, wait for the running ones, then exit.
    Stop {
        /// End the calls now instead of waiting for them.
        #[serde(default)]
        force: bool,
        /// Seconds to wait for the calls (default: `[service] stop_timeout_secs`).
        #[serde(default)]
        timeout: Option<u64>,
    },
    /// Stream call events until the client closes the connection.
    Subscribe,
}

/// One trunk in the status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrunkStatus {
    /// Trunk name.
    pub name: String,
    /// `none` (the trunk does not register), `registering`, `registered`, `failed: why`, ...
    pub registration: String,
}

/// What `status` answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusInfo {
    /// Process id.
    pub pid: u32,
    /// `sip-faxd` version.
    pub version: String,
    /// `running`, or `stopping` while the calls are drained.
    pub state: String,
    /// Start time, RFC 3339 (UTC).
    pub started_at: String,
    /// Seconds since the start.
    pub uptime_secs: u64,
    /// The SIP listeners (`udp 0.0.0.0:5060`).
    pub listen: Vec<String>,
    /// Calls in progress.
    pub active_calls: usize,
    /// The trunks.
    pub trunks: Vec<TrunkStatus>,
}

/// A call in progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallInfo {
    /// SIP Call-ID.
    pub call_id: String,
    /// Trunk name.
    pub trunk: String,
    /// Source address.
    pub peer: String,
    /// Calling number (`""` when unknown).
    pub ani: String,
    /// Called number.
    pub dnis: String,
    /// INVITE received, RFC 3339 (UTC).
    pub started_at: String,
    /// 200 sent.
    pub answered: bool,
    /// T.38 in force.
    pub t38: bool,
    /// Pages received so far.
    pub pages: u32,
}

/// What `subscribe` streams.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// A call began.
    CallStarted {
        /// The call.
        call: CallInfo,
    },
    /// 200 sent.
    CallAnswered {
        /// Call-ID.
        call_id: String,
    },
    /// T.38 is in force: the fax session starts.
    FaxStarted {
        /// Call-ID.
        call_id: String,
    },
    /// T.30 confirmed another page.
    FaxProgress {
        /// Call-ID.
        call_id: String,
        /// Pages so far.
        pages: u32,
    },
    /// The call is over.
    CallFinished {
        /// Call-ID.
        call_id: String,
        /// `completed` or `failed`.
        status: String,
        /// Pages received.
        pages: u32,
        /// `received`, `partial`, ... as the fax list will show it.
        fax_status: String,
        /// Why it ended (`completed`, `shutdown`, ...).
        hangup_code: String,
    },
    /// The service is stopping.
    Stopping {
        /// Calls still running.
        active: usize,
    },
}

/// A reply line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Reply {
    /// Answer to `status`.
    Status(StatusInfo),
    /// Answer to `calls`.
    Calls {
        /// The calls.
        calls: Vec<CallInfo>,
    },
    /// Answer to `reload`.
    Reload {
        /// What was reloaded.
        message: String,
    },
    /// First line of the answer to `stop`.
    Stopping {
        /// Calls running now.
        active: usize,
        /// Seconds the service waits for them (0 with `force`).
        timeout_secs: u64,
        /// The stop is forced.
        force: bool,
    },
    /// While a stop waits: how many calls are left.
    Waiting {
        /// Calls running.
        active: usize,
        /// Seconds until they are ended.
        remaining_secs: u64,
    },
    /// The last line of the answer to `stop`: everything is shut down, the process exits.
    Stopped,
    /// First line of the answer to `subscribe`.
    Subscribed {
        /// The calls running now.
        calls: Vec<CallInfo>,
    },
    /// A streamed event.
    Event {
        /// What happened.
        event: Event,
    },
    /// The request failed or could not be understood.
    Error {
        /// Why.
        message: String,
    },
}

impl Reply {
    /// The line (without the newline).
    pub fn to_line(&self) -> String {
        serde_json::to_string(self)
            .unwrap_or_else(|_| r#"{"type":"error","message":"encode"}"#.into())
    }
}

#[cfg(test)]
mod tests;
