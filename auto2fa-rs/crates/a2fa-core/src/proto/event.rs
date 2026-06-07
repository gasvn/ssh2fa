#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    HostStatusChanged,
    TunnelStatusChanged,
    Notification,
}

impl Event {
    pub fn as_str(&self) -> &'static str {
        match self {
            Event::HostStatusChanged => "host_status_changed",
            Event::TunnelStatusChanged => "tunnel_status_changed",
            Event::Notification => "notification",
        }
    }
}
