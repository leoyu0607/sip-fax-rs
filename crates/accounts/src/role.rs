//! Roles and what they may do (docs/design/m4-web.md section 2.1).

use std::fmt;

/// An account role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    /// Operator of the fax inbox: reads faxes, marks them read, writes notes.
    Agent,
    /// Administrator: everything.
    Admin,
}

impl Role {
    /// Stored / API name.
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Agent => "agent",
            Role::Admin => "admin",
        }
    }

    /// Parses `admin` or `agent`.
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "admin" => Some(Role::Admin),
            "agent" => Some(Role::Agent),
            _ => None,
        }
    }

    /// Whether the role may do `cap`.
    pub fn can(self, cap: Capability) -> bool {
        match self {
            Role::Admin => true,
            Role::Agent => matches!(
                cap,
                Capability::ViewFaxes
                    | Capability::EditFaxMeta
                    | Capability::ViewDeliveries
                    | Capability::ViewStatus
            ),
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One thing a request needs permission for. The REST layer maps every route to exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    /// Fax list, details, previews, downloads.
    ViewFaxes,
    /// Read / unread toggle and the note.
    EditFaxMeta,
    /// Delivery attempts of a fax.
    ViewDeliveries,
    /// Queue a failed delivery again.
    Redeliver,
    /// Read the configuration sections.
    ViewConfig,
    /// Change any configuration.
    EditConfig,
    /// Create, change, disable, delete accounts.
    ManageUsers,
    /// The configuration audit trail.
    ViewAudit,
    /// Version, uptime, calls in progress, registrations, pending restart.
    ViewStatus,
}

impl Capability {
    /// Every capability (for table-driven tests and documentation).
    pub const ALL: [Capability; 9] = [
        Capability::ViewFaxes,
        Capability::EditFaxMeta,
        Capability::ViewDeliveries,
        Capability::Redeliver,
        Capability::ViewConfig,
        Capability::EditConfig,
        Capability::ManageUsers,
        Capability::ViewAudit,
        Capability::ViewStatus,
    ];
}
