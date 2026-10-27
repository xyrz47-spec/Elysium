//! 페어링 링크 읽기: elysium://pair?v=1&h=주소1,주소2&p=포트&f=지문&c=코드&n=PC이름

#[derive(Debug, PartialEq)]
pub struct Link {
    pub hosts: Vec<String>,
    pub port: u16,
    pub fingerprint: String,
    pub code: String,
    pub name: String,
}

pub fn parse(link: &str) -> Result<Link, String> {
    let bad = || "엘리시움 페어링 QR이 아닙니다. PC의 설정 → 폰 연결에서 띄운 QR을 찍으세요".to_string();
    let q = link.trim().strip_prefix("elysium://pair?").ok_or_else(bad)?;
    let get = |k: &str| q.split('&').find_map(|kv| kv.strip_prefix(&format!("{k}="))).unwrap_or("").to_string();
    let hosts: Vec<String> = get("h")
        .split(',')
        .map(str::trim)
        // IPv4 주소만 받는다(링크에 다른 곳을 끼워 넣지 못하게).
        .filter(|h| h.parse::<std::net::Ipv4Addr>().is_ok())
        .map(str::to_string)
        .collect();
    let port: u16 = get("p").parse().map_err(|_| bad())?;
    let fingerprint = get("f").to_ascii_lowercase();
    let code = get("c");
    if hosts.is_empty() || fingerprint.len() != 64 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) || code.is_empty() {
        return Err(bad());
    }
    Ok(Link { hosts, port, fingerprint, code, name: get("n") })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_link_and_rejects_others() {
        let fp = "a".repeat(64);
        let l = parse(&format!("elysium://pair?v=1&h=192.168.0.5,100.80.1.2,evil.com&p=47820&f={fp}&c=abc&n=HOYA-PC")).unwrap();
        assert_eq!(l.hosts, vec!["192.168.0.5", "100.80.1.2"]);
        assert_eq!((l.port, l.code.as_str(), l.name.as_str()), (47820, "abc", "HOYA-PC"));
        assert!(parse("https://example.com").is_err());
        assert!(parse(&format!("elysium://pair?h=1.2.3.4&p=1&f=short&c=x")).is_err());
    }
}
