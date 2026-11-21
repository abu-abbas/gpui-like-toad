//! Redis console helpers: split a typed command line into arguments the way
//! `redis-cli` does, and sort commands into what the console may run.

/// Split a command line into arguments. Double quotes allow spaces and the
/// usual escapes (`\n`, `\t`, `\"`, `\\`, `\xHH`); single quotes are taken
/// literally except for `\'`.
pub fn split_args(line: &str) -> Result<Vec<Vec<u8>>, String> {
    let mut args = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let Some(&first) = chars.peek() else {
            break;
        };
        let mut arg = Vec::<u8>::new();
        let push = |arg: &mut Vec<u8>, c: char| {
            let mut buf = [0u8; 4];
            arg.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        };
        if first == '"' {
            chars.next();
            let mut closed = false;
            while let Some(c) = chars.next() {
                match c {
                    '"' => {
                        closed = true;
                        break;
                    }
                    '\\' => match chars.next() {
                        Some('n') => arg.push(b'\n'),
                        Some('r') => arg.push(b'\r'),
                        Some('t') => arg.push(b'\t'),
                        Some('a') => arg.push(7),
                        Some('b') => arg.push(8),
                        Some('x') => {
                            let hex: String = chars.by_ref().take(2).collect();
                            match u8::from_str_radix(&hex, 16) {
                                Ok(b) if hex.len() == 2 => arg.push(b),
                                _ => return Err(format!("Invalid escape \\x{hex}")),
                            }
                        }
                        Some(other) => push(&mut arg, other),
                        None => return Err("Unbalanced quotes".into()),
                    },
                    other => push(&mut arg, other),
                }
            }
            if !closed {
                return Err("Unbalanced quotes".into());
            }
        } else if first == '\'' {
            chars.next();
            let mut closed = false;
            while let Some(c) = chars.next() {
                match c {
                    '\'' => {
                        closed = true;
                        break;
                    }
                    '\\' if chars.peek() == Some(&'\'') => {
                        chars.next();
                        arg.push(b'\'');
                    }
                    other => push(&mut arg, other),
                }
            }
            if !closed {
                return Err("Unbalanced quotes".into());
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                chars.next();
                push(&mut arg, c);
            }
        }
        if chars.peek().is_some_and(|c| !c.is_whitespace()) {
            return Err("Closing quote must be followed by a space".into());
        }
        args.push(arg);
    }
    Ok(args)
}

/// What the console does with a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandClass {
    /// Never run from the console: it would block the connection
    /// (MONITOR, SUBSCRIBE) or switch its protocol.
    Unsupported,
    /// Wipes or reconfigures the server: only with "Allow dangerous
    /// commands" on the instance.
    Dangerous,
    /// Reads only; allowed on a read-only (PROD) instance.
    Read,
    /// Anything else (writes, unknown commands).
    Write,
}

const UNSUPPORTED: &[&str] = &[
    "MONITOR",
    "SUBSCRIBE",
    "PSUBSCRIBE",
    "SSUBSCRIBE",
    "SYNC",
    "PSYNC",
    "HELLO",
    "RESET",
    "QUIT",
    "SELECT",
    "CLIENT",
    "WAIT",
    "WAITAOF",
];

const DANGEROUS: &[&str] = &[
    "FLUSHDB",
    "FLUSHALL",
    "KEYS",
    "SHUTDOWN",
    "DEBUG",
    "CONFIG",
    "REPLICAOF",
    "SLAVEOF",
    "MIGRATE",
    "CLUSTER",
    "FAILOVER",
    "SAVE",
    "BGSAVE",
    "BGREWRITEAOF",
    "SWAPDB",
    "MODULE",
    "ACL",
    "SCRIPT",
    "FUNCTION",
];

const READ: &[&str] = &[
    "PING",
    "ECHO",
    "INFO",
    "DBSIZE",
    "TIME",
    "LASTSAVE",
    "TYPE",
    "TTL",
    "PTTL",
    "EXPIRETIME",
    "PEXPIRETIME",
    "EXISTS",
    "SCAN",
    "RANDOMKEY",
    "OBJECT",
    "MEMORY",
    "STRLEN",
    "GET",
    "MGET",
    "GETRANGE",
    "SUBSTR",
    "LCS",
    "HGET",
    "HMGET",
    "HGETALL",
    "HKEYS",
    "HVALS",
    "HLEN",
    "HEXISTS",
    "HSTRLEN",
    "HSCAN",
    "HRANDFIELD",
    "LRANGE",
    "LINDEX",
    "LLEN",
    "LPOS",
    "SMEMBERS",
    "SCARD",
    "SISMEMBER",
    "SMISMEMBER",
    "SSCAN",
    "SRANDMEMBER",
    "SINTER",
    "SUNION",
    "SDIFF",
    "SINTERCARD",
    "ZRANGE",
    "ZREVRANGE",
    "ZRANGEBYSCORE",
    "ZREVRANGEBYSCORE",
    "ZRANGEBYLEX",
    "ZREVRANGEBYLEX",
    "ZSCORE",
    "ZMSCORE",
    "ZRANK",
    "ZREVRANK",
    "ZCARD",
    "ZCOUNT",
    "ZLEXCOUNT",
    "ZSCAN",
    "ZRANDMEMBER",
    "ZINTER",
    "ZUNION",
    "ZDIFF",
    "ZINTERCARD",
    "XRANGE",
    "XREVRANGE",
    "XLEN",
    "XINFO",
    "XPENDING",
    "PFCOUNT",
    "GETBIT",
    "BITCOUNT",
    "BITPOS",
    "GEOPOS",
    "GEODIST",
    "GEOHASH",
    "GEOSEARCH",
    "DUMP",
    "TOUCH",
    "COMMAND",
    "ROLE",
    "LOLWUT",
    "SLOWLOG",
    "LATENCY",
];

/// Class of a command by its name (first argument).
pub fn classify(name: &str) -> CommandClass {
    let upper = name.to_ascii_uppercase();
    let name = upper.as_str();
    if UNSUPPORTED.contains(&name) {
        CommandClass::Unsupported
    } else if DANGEROUS.contains(&name) {
        CommandClass::Dangerous
    } else if READ.contains(&name) {
        CommandClass::Read
    } else {
        CommandClass::Write
    }
}

/// Why the console refuses `args` on an instance, or `None` when it may run.
pub fn refusal(args: &[Vec<u8>], allow_dangerous: bool, read_only: bool) -> Option<String> {
    let name = String::from_utf8_lossy(args.first()?).to_ascii_uppercase();
    match classify(&name) {
        CommandClass::Unsupported if name == "SELECT" => {
            Some("Use the database dropdown to switch databases.".into())
        }
        CommandClass::Unsupported => Some(format!("{name} is not supported in the console.")),
        CommandClass::Dangerous if !allow_dangerous => Some(format!(
            "Blocked: {name} is a dangerous command. Turn on \"Allow dangerous commands\" for this instance to run it."
        )),
        CommandClass::Read => None,
        _ if read_only => Some(format!(
            "Blocked: this instance is read only (PROD), {name} may write."
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(args: Vec<Vec<u8>>) -> Vec<String> {
        args.into_iter()
            .map(|a| String::from_utf8(a).unwrap())
            .collect()
    }

    #[test]
    fn splits_plain_and_quoted() {
        assert_eq!(
            s(split_args(r#"SET "a b" 'c d'  e"#).unwrap()),
            ["SET", "a b", "c d", "e"]
        );
        assert_eq!(
            s(split_args(r#"SET k "x\ny""#).unwrap()),
            ["SET", "k", "x\ny"]
        );
        assert_eq!(split_args(r#"SET k "\x41""#).unwrap()[2], b"A");
        assert!(split_args(r#"SET "k"#).is_err());
        assert!(split_args(r#"SET "k"x"#).is_err());
        assert!(split_args("   ").unwrap().is_empty());
    }

    #[test]
    fn refuses_by_class() {
        let args = |l: &str| split_args(l).unwrap();
        assert!(refusal(&args("flushdb"), false, false).is_some());
        assert!(refusal(&args("flushdb"), true, false).is_none());
        assert!(refusal(&args("monitor"), true, false).is_some());
        assert!(refusal(&args("select 2"), true, false).is_some());
        assert!(refusal(&args("get k"), false, true).is_none());
        assert!(refusal(&args("set k v"), false, true).is_some());
        assert!(refusal(&args("set k v"), false, false).is_none());
    }
}
