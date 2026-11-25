//! Saved Developer Tools instances (Redis, MailHog). Kept apart from
//! database connections in `tools.json`: tools are not SQL connections and
//! have their own section in the sidebar. Passwords are never written
//! here; with `save_password` they live in the keychain under the id.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Redis,
    MailHog,
}

impl ToolKind {
    pub fn key(self) -> &'static str {
        match self {
            ToolKind::Redis => "redis",
            ToolKind::MailHog => "mailhog",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "redis" => Some(ToolKind::Redis),
            "mailhog" => Some(ToolKind::MailHog),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ToolKind::Redis => "Redis",
            ToolKind::MailHog => "MailHog",
        }
    }

    pub fn default_port(self) -> u16 {
        match self {
            ToolKind::Redis => 6379,
            ToolKind::MailHog => 8025,
        }
    }
}

/// One saved tool instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInstance {
    /// Stable id: the keychain account for this instance's password.
    pub id: String,
    pub kind: ToolKind,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub save_password: bool,
    /// Redis: TLS. MailHog: HTTPS.
    pub tls: bool,
    /// Redis: database opened first.
    pub database: u32,
    /// Redis: let the console run FLUSHDB, KEYS, CONFIG and the like.
    pub allow_dangerous: bool,
    pub environment: ConnectionEnvironment,
    /// Kilat starts the server itself (local hosts only) and stops it
    /// when Kilat quits.
    pub start_server: bool,
    /// The command that starts the server; empty means the default.
    pub server_command: String,
}

impl ToolInstance {
    pub fn new(kind: ToolKind) -> Self {
        Self {
            id: new_tool_id(),
            kind,
            name: String::new(),
            host: "localhost".into(),
            port: kind.default_port(),
            username: String::new(),
            save_password: false,
            tls: false,
            database: 0,
            allow_dangerous: false,
            environment: ConnectionEnvironment::None,
            start_server: false,
            server_command: String::new(),
        }
    }

    /// PROD instances are read only: writes and deletes are refused.
    pub fn read_only(&self) -> bool {
        self.environment == ConnectionEnvironment::Production
    }

    pub fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// A new unique id, like profile ids but prefixed `t`.
pub fn new_tool_id() -> String {
    let id = new_profile_id();
    format!("t{}", &id[1..])
}

pub(super) fn tool_to_json(t: &ToolInstance) -> Json {
    Json::Obj(vec![
        ("id".into(), Json::Str(t.id.clone())),
        ("kind".into(), Json::Str(t.kind.key().into())),
        ("name".into(), Json::Str(t.name.clone())),
        ("host".into(), Json::Str(t.host.clone())),
        ("port".into(), Json::Num(f64::from(t.port))),
        ("username".into(), Json::Str(t.username.clone())),
        ("save_password".into(), Json::Bool(t.save_password)),
        ("tls".into(), Json::Bool(t.tls)),
        ("database".into(), Json::Num(f64::from(t.database))),
        ("allow_dangerous".into(), Json::Bool(t.allow_dangerous)),
        (
            "environment".into(),
            Json::Str(t.environment.label().into()),
        ),
        ("start_server".into(), Json::Bool(t.start_server)),
        ("server_command".into(), Json::Str(t.server_command.clone())),
    ])
}

pub(super) fn tool_from_json(v: &Json) -> Option<ToolInstance> {
    let kind = ToolKind::from_key(v.get("kind")?.as_str()?)?;
    let s = |k: &str| {
        v.get(k)
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let b = |k: &str| v.get(k).and_then(Json::as_bool).unwrap_or(false);
    Some(ToolInstance {
        id: s("id"),
        kind,
        name: s("name"),
        host: v
            .get("host")
            .and_then(Json::as_str)
            .filter(|h| !h.is_empty())
            .unwrap_or("localhost")
            .to_string(),
        port: v
            .get("port")
            .and_then(Json::as_u64)
            .and_then(|p| u16::try_from(p).ok())
            .filter(|p| *p != 0)
            .unwrap_or(kind.default_port()),
        username: s("username"),
        save_password: b("save_password"),
        tls: b("tls"),
        database: v
            .get("database")
            .and_then(Json::as_u64)
            .and_then(|d| u32::try_from(d).ok())
            .unwrap_or(0),
        allow_dangerous: b("allow_dangerous"),
        environment: ConnectionEnvironment::parse(&s("environment")),
        start_server: b("start_server"),
        server_command: s("server_command"),
    })
}

pub fn load_tools_in(dir: &Path) -> Vec<ToolInstance> {
    let mut tools: Vec<ToolInstance> = match read_json_file(&dir.join("tools.json")) {
        Some(Json::Arr(items)) => items.iter().filter_map(tool_from_json).collect(),
        _ => Vec::new(),
    };
    let mut seen = std::collections::HashSet::new();
    let mut changed = false;
    for t in &mut tools {
        if t.id.is_empty() || !seen.insert(t.id.clone()) {
            t.id = new_tool_id();
            seen.insert(t.id.clone());
            changed = true;
        }
    }
    if changed {
        let _ = save_tools_in(dir, &tools);
    }
    tools
}

pub fn save_tools_in(dir: &Path, tools: &[ToolInstance]) -> std::io::Result<()> {
    write_json_file(
        dir,
        "tools.json",
        &Json::Arr(tools.iter().map(tool_to_json).collect()),
    )
}

pub fn load_tools() -> Vec<ToolInstance> {
    load_tools_in(&config_dir())
}

pub fn save_tools(tools: &[ToolInstance]) -> std::io::Result<()> {
    save_tools_in(&config_dir(), tools)
}

#[cfg(test)]
mod tool_tests {
    use super::*;

    #[test]
    fn round_trip_and_repair() {
        // nosemgrep: rust.lang.security.temp-dir.temp-dir
        let dir = std::env::temp_dir().join(format!("kilat-tools-{}", new_tool_id()));
        let mut redis = ToolInstance::new(ToolKind::Redis);
        redis.name = "Local cache".into();
        redis.database = 3;
        redis.allow_dangerous = true;
        redis.environment = ConnectionEnvironment::Production;
        redis.start_server = true;
        redis.server_command = "redis-server --port 6390".into();
        let mut mh = ToolInstance::new(ToolKind::MailHog);
        mh.id = redis.id.clone(); // duplicate id gets repaired
        mh.port = 8026;
        save_tools_in(&dir, &[redis.clone(), mh]).unwrap();
        let loaded = load_tools_in(&dir);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0], redis);
        assert!(loaded[0].read_only());
        assert_eq!(loaded[1].kind, ToolKind::MailHog);
        assert_eq!(loaded[1].port, 8026);
        assert_ne!(loaded[1].id, redis.id);
        assert!(loaded[1].id.starts_with('t'));
        // Unknown kinds are skipped, not fatal.
        std::fs::write(
            dir.join("tools.json"),
            r#"[{"kind":"kafka"},{"kind":"redis"}]"#,
        )
        .unwrap();
        let loaded = load_tools_in(&dir);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].port, 6379);
        let _ = std::fs::remove_dir_all(dir);
    }
}
