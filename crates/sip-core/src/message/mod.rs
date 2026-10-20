//! SIP message model: [`Request`], [`Response`], [`SipMessage`], [`Method`], [`StatusCode`], and
//! the typed header values, plus [`parse_message`] and serialization.

mod cseq;
mod headers;
mod lex;
mod name_addr;
mod parse;
mod serialize;
mod uri;
mod via;

use std::fmt;

use bytes::Bytes;

pub use cseq::CSeq;
pub use headers::{COMPACT_FORMS, Header, Headers, canonical_name, compact_letter};
pub use lex::{Param, find_param};
pub use name_addr::NameAddr;
pub use parse::{
    Framing, MAX_HEADERS, MAX_MESSAGE_SIZE, ParseError, parse_datagram, parse_message,
    parse_with_framing,
};
pub use uri::{SipUri, Uri, UriScheme, escape_user, unescape};
pub use via::Via;

/// A SIP request method. Method names are case-sensitive (RFC 3261 section 7.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Method {
    /// INVITE
    Invite,
    /// ACK
    Ack,
    /// BYE
    Bye,
    /// CANCEL
    Cancel,
    /// OPTIONS
    Options,
    /// REGISTER
    Register,
    /// INFO
    Info,
    /// UPDATE
    Update,
    /// PRACK
    Prack,
    /// SUBSCRIBE
    Subscribe,
    /// NOTIFY
    Notify,
    /// REFER
    Refer,
    /// MESSAGE
    Message,
    /// PUBLISH
    Publish,
    /// Any other (extension) method token, e.g. `NEWMETHOD`.
    Other(String),
}

impl Method {
    /// Maps a method token onto the enum. The token must already be validated.
    pub fn from_token(s: &str) -> Method {
        match s {
            "INVITE" => Method::Invite,
            "ACK" => Method::Ack,
            "BYE" => Method::Bye,
            "CANCEL" => Method::Cancel,
            "OPTIONS" => Method::Options,
            "REGISTER" => Method::Register,
            "INFO" => Method::Info,
            "UPDATE" => Method::Update,
            "PRACK" => Method::Prack,
            "SUBSCRIBE" => Method::Subscribe,
            "NOTIFY" => Method::Notify,
            "REFER" => Method::Refer,
            "MESSAGE" => Method::Message,
            "PUBLISH" => Method::Publish,
            other => Method::Other(other.to_string()),
        }
    }

    /// The method as written on the wire.
    pub fn as_str(&self) -> &str {
        match self {
            Method::Invite => "INVITE",
            Method::Ack => "ACK",
            Method::Bye => "BYE",
            Method::Cancel => "CANCEL",
            Method::Options => "OPTIONS",
            Method::Register => "REGISTER",
            Method::Info => "INFO",
            Method::Update => "UPDATE",
            Method::Prack => "PRACK",
            Method::Subscribe => "SUBSCRIBE",
            Method::Notify => "NOTIFY",
            Method::Refer => "REFER",
            Method::Message => "MESSAGE",
            Method::Publish => "PUBLISH",
            Method::Other(s) => s,
        }
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A SIP status code in `100 ..= 699`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StatusCode(u16);

macro_rules! status_codes {
    ($($name:ident = $code:expr, $text:expr;)*) => {
        impl StatusCode {
            $(#[doc = $text] pub const $name: StatusCode = StatusCode($code);)*

            /// Default reason phrase for well-known codes.
            pub fn reason_phrase(self) -> &'static str {
                match self.0 {
                    $($code => $text,)*
                    100..=199 => "Provisional",
                    200..=299 => "Success",
                    300..=399 => "Redirection",
                    400..=499 => "Client Error",
                    500..=599 => "Server Error",
                    _ => "Global Failure",
                }
            }
        }
    };
}

status_codes! {
    TRYING = 100, "Trying";
    RINGING = 180, "Ringing";
    SESSION_PROGRESS = 183, "Session Progress";
    OK = 200, "OK";
    BAD_REQUEST = 400, "Bad Request";
    UNAUTHORIZED = 401, "Unauthorized";
    NOT_FOUND = 404, "Not Found";
    METHOD_NOT_ALLOWED = 405, "Method Not Allowed";
    REQUEST_TIMEOUT = 408, "Request Timeout";
    UNSUPPORTED_MEDIA_TYPE = 415, "Unsupported Media Type";
    BAD_EXTENSION = 420, "Bad Extension";
    SESSION_INTERVAL_TOO_SMALL = 422, "Session Interval Too Small";
    CALL_DOES_NOT_EXIST = 481, "Call/Transaction Does Not Exist";
    LOOP_DETECTED = 482, "Loop Detected";
    TOO_MANY_HOPS = 483, "Too Many Hops";
    BUSY_HERE = 486, "Busy Here";
    REQUEST_TERMINATED = 487, "Request Terminated";
    NOT_ACCEPTABLE_HERE = 488, "Not Acceptable Here";
    REQUEST_PENDING = 491, "Request Pending";
    SERVER_INTERNAL_ERROR = 500, "Server Internal Error";
    NOT_IMPLEMENTED = 501, "Not Implemented";
    SERVICE_UNAVAILABLE = 503, "Service Unavailable";
    VERSION_NOT_SUPPORTED = 505, "Version Not Supported";
    DECLINE = 603, "Decline";
}

impl StatusCode {
    /// Creates a status code; `None` unless `100 <= code <= 699`.
    pub fn new(code: u16) -> Option<Self> {
        (100..=699).contains(&code).then_some(StatusCode(code))
    }

    /// Numeric value.
    pub fn code(self) -> u16 {
        self.0
    }

    /// 1xx
    pub fn is_provisional(self) -> bool {
        self.0 < 200
    }

    /// 2xx
    pub fn is_success(self) -> bool {
        (200..300).contains(&self.0)
    }

    /// 200 and above.
    pub fn is_final(self) -> bool {
        self.0 >= 200
    }
}

impl fmt::Display for StatusCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A SIP request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// Request method.
    pub method: Method,
    /// Request-URI.
    pub uri: Uri,
    /// Headers in wire order.
    pub headers: Headers,
    /// Message body.
    pub body: Bytes,
}

impl Request {
    /// Creates a request without headers or body.
    pub fn new(method: Method, uri: impl Into<Uri>) -> Self {
        Request {
            method,
            uri: uri.into(),
            headers: Headers::new(),
            body: Bytes::new(),
        }
    }

    /// `true` when the CSeq method equals the request-line method. RFC 4475 3.1.2.17 /
    /// 3.1.2.18: a mismatch parses fine but the receiving UAS must answer 400 (or 501 for an
    /// unknown method). That decision belongs to the transaction/UAS layer.
    pub fn cseq_method_matches(&self) -> bool {
        self.headers
            .cseq()
            .map(|c| c.method == self.method)
            .unwrap_or(false)
    }
}

/// A SIP response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// Status code.
    pub status: StatusCode,
    /// Reason phrase as received (may be empty).
    pub reason: String,
    /// Headers in wire order.
    pub headers: Headers,
    /// Message body.
    pub body: Bytes,
}

impl Response {
    /// Creates a response with the default reason phrase and no headers or body.
    pub fn new(status: StatusCode) -> Self {
        Response {
            status,
            reason: status.reason_phrase().to_string(),
            headers: Headers::new(),
            body: Bytes::new(),
        }
    }
}

/// Either a request or a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SipMessage {
    /// A request.
    Request(Request),
    /// A response.
    Response(Response),
}

impl SipMessage {
    /// Headers of either variant.
    pub fn headers(&self) -> &Headers {
        match self {
            SipMessage::Request(r) => &r.headers,
            SipMessage::Response(r) => &r.headers,
        }
    }

    /// Body of either variant.
    pub fn body(&self) -> &Bytes {
        match self {
            SipMessage::Request(r) => &r.body,
            SipMessage::Response(r) => &r.body,
        }
    }
}
