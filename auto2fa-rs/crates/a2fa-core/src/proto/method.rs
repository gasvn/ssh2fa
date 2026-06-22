#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Ping,
    ListHosts,
    ListTunnels,
    HostToggle,
    HostMountToggle,
    HostRotate,
    HostAdd,
    HostTestCredentials,
    HostTotp,
    TunnelAdd,
    TunnelRemove,
    TunnelToggle,
    TunnelStart,
    TunnelStop,
    TunnelSetNode,
    TunnelSetAutostart,
    TunnelSetJumpCandidates,
    TunnelSetPostConnect,
    TunnelSetTags,
    TunnelSetUrlPath,
    TunnelRename,
    TunnelsBatch,
    DiscoverNodes,
    PortSuggest,
    WakeRecover,
    ResetAll,
    LogTail,
    TunnelEvents,
    SubscribeEvents,
}

impl Method {
    pub fn as_str(&self) -> &'static str {
        match self {
            Method::Ping => "ping",
            Method::ListHosts => "list_hosts",
            Method::ListTunnels => "list_tunnels",
            Method::HostToggle => "host_toggle",
            Method::HostMountToggle => "host_mount_toggle",
            Method::HostRotate => "host_rotate",
            Method::HostAdd => "host_add",
            Method::HostTestCredentials => "host_test_credentials",
            Method::HostTotp => "host_totp",
            Method::TunnelAdd => "tunnel_add",
            Method::TunnelRemove => "tunnel_remove",
            Method::TunnelToggle => "tunnel_toggle",
            Method::TunnelStart => "tunnel_start",
            Method::TunnelStop => "tunnel_stop",
            Method::TunnelSetNode => "tunnel_set_node",
            Method::TunnelSetAutostart => "tunnel_set_autostart",
            Method::TunnelSetJumpCandidates => "tunnel_set_jump_candidates",
            Method::TunnelSetPostConnect => "tunnel_set_post_connect",
            Method::TunnelSetTags => "tunnel_set_tags",
            Method::TunnelSetUrlPath => "tunnel_set_url_path",
            Method::TunnelRename => "tunnel_rename",
            Method::TunnelsBatch => "tunnels_batch",
            Method::DiscoverNodes => "discover_nodes",
            Method::PortSuggest => "port_suggest",
            Method::WakeRecover => "wake_recover",
            Method::ResetAll => "reset_all",
            Method::LogTail => "log_tail",
            Method::TunnelEvents => "tunnel_events",
            Method::SubscribeEvents => "subscribe_events",
        }
    }

    #[allow(clippy::should_implement_trait)] // intentional custom parser (returns Option), not std FromStr
    pub fn from_str(s: &str) -> Option<Method> {
        match s {
            "ping" => Some(Method::Ping),
            "list_hosts" => Some(Method::ListHosts),
            "list_tunnels" => Some(Method::ListTunnels),
            "host_toggle" => Some(Method::HostToggle),
            "host_mount_toggle" => Some(Method::HostMountToggle),
            "host_rotate" => Some(Method::HostRotate),
            "host_add" => Some(Method::HostAdd),
            "host_test_credentials" => Some(Method::HostTestCredentials),
            "host_totp" => Some(Method::HostTotp),
            "tunnel_add" => Some(Method::TunnelAdd),
            "tunnel_remove" => Some(Method::TunnelRemove),
            "tunnel_toggle" => Some(Method::TunnelToggle),
            "tunnel_start" => Some(Method::TunnelStart),
            "tunnel_stop" => Some(Method::TunnelStop),
            "tunnel_set_node" => Some(Method::TunnelSetNode),
            "tunnel_set_autostart" => Some(Method::TunnelSetAutostart),
            "tunnel_set_jump_candidates" => Some(Method::TunnelSetJumpCandidates),
            "tunnel_set_post_connect" => Some(Method::TunnelSetPostConnect),
            "tunnel_set_tags" => Some(Method::TunnelSetTags),
            "tunnel_set_url_path" => Some(Method::TunnelSetUrlPath),
            "tunnel_rename" => Some(Method::TunnelRename),
            "tunnels_batch" => Some(Method::TunnelsBatch),
            "discover_nodes" => Some(Method::DiscoverNodes),
            "port_suggest" => Some(Method::PortSuggest),
            "wake_recover" => Some(Method::WakeRecover),
            "reset_all" => Some(Method::ResetAll),
            "log_tail" => Some(Method::LogTail),
            "tunnel_events" => Some(Method::TunnelEvents),
            "subscribe_events" => Some(Method::SubscribeEvents),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_strings_match_python() {
        assert_eq!(Method::ListHosts.as_str(), "list_hosts");
        assert_eq!(Method::TunnelSetJumpCandidates.as_str(), "tunnel_set_jump_candidates");
        assert_eq!(Method::from_str("host_add"), Some(Method::HostAdd));
        assert_eq!(Method::from_str("nope"), None);
    }
}
