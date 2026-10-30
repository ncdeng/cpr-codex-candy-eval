//! 降智状态变化的邮件通知：直连 SMTP（465 隐式 TLS 或 587 STARTTLS），只发给设置里的收件人。
//!
//! 授权码只从宿主下发的敏感字段读取，只用于 AUTH，不进日志、不进错误信息。
use base64::Engine;
use rustls::pki_types::ServerName;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

const TIMEOUT: Duration = Duration::from_secs(30);

/// Not Debug: the password must never be formatted.
pub struct MailConfig {
    host: String,
    port: u16,
    username: String,
    password: String,
    pub to: Vec<String>,
}

fn text<'a>(config: &'a Value, field: &str) -> &'a str {
    config
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
}

/// 地址里不能有换行、尖括号或空白，避免改写邮件头。
fn valid_address(address: &str) -> bool {
    let mut parts = address.split('@');
    matches!((parts.next(), parts.next(), parts.next()), (Some(user), Some(domain), None)
        if !user.is_empty() && domain.contains('.'))
        && !address
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "<>,;\"".contains(c))
}

/// 邮件通知关闭时返回 `Ok(None)`；开启但设置不完整时返回原因。
pub fn mail_config(config: &Value) -> Result<Option<MailConfig>, String> {
    if config.get("email_enabled").and_then(Value::as_bool) != Some(true) {
        return Ok(None);
    }
    let host = text(config, "smtp_host");
    let port = match text(config, "smtp_port") {
        "" | "465" => 465,
        "587" => 587,
        _ => return Err("SMTP 端口只支持 465 或 587".to_owned()),
    };
    let username = text(config, "smtp_username");
    let password = config
        .get("smtp_password")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let to: Vec<String> = text(config, "email_to")
        .split(|c: char| matches!(c, ',' | '，' | ';' | '；') || c.is_whitespace())
        .map(str::trim)
        .filter(|address| !address.is_empty())
        .map(str::to_owned)
        .collect();
    if host.is_empty() || host.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("请填写 SMTP 服务器地址".to_owned());
    }
    if !valid_address(username) {
        return Err("SMTP 账号必须是完整的发件邮箱地址".to_owned());
    }
    if password.is_empty() {
        return Err(
            "还没填 SMTP 授权码：请到 CPR「插件管理」→ 本插件 →「设置」里填写（CPR 加密保存）".to_owned(),
        );
    }
    if to.is_empty() || to.len() > 10 || !to.iter().all(|address| valid_address(address)) {
        return Err("收件人要填 1–10 个有效邮箱，用逗号分隔".to_owned());
    }
    Ok(Some(MailConfig {
        host: host.to_owned(),
        port,
        username: username.to_owned(),
        password: password.to_owned(),
        to,
    }))
}

fn tls() -> Result<TlsConnector, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|_| "TLS 初始化失败".to_owned())?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

fn io_error(_: std::io::Error) -> String {
    "与邮件服务器的连接中断".to_owned()
}

struct Conn<S> {
    io: BufReader<S>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Conn<S> {
    async fn reply(&mut self) -> Result<(u16, String), String> {
        let mut message = String::new();
        loop {
            let mut line = String::new();
            if self.io.read_line(&mut line).await.map_err(io_error)? == 0 {
                return Err("邮件服务器断开了连接".to_owned());
            }
            let code = line
                .get(..3)
                .and_then(|code| code.parse::<u16>().ok())
                .ok_or_else(|| "邮件服务器的回复格式不对".to_owned())?;
            message.push_str(line.get(4..).unwrap_or_default().trim_end());
            if line.as_bytes().get(3) != Some(&b'-') {
                return Ok((code, message));
            }
            message.push(' ');
        }
    }

    /// `label` 用在错误信息里，代替可能含凭据的原始命令。
    async fn command(&mut self, line: &str, label: &str, expect: &[u16]) -> Result<(), String> {
        let stream = self.io.get_mut();
        stream
            .write_all(format!("{line}\r\n").as_bytes())
            .await
            .map_err(io_error)?;
        stream.flush().await.map_err(io_error)?;
        self.expect(label, expect).await
    }

    async fn expect(&mut self, label: &str, expect: &[u16]) -> Result<(), String> {
        let (code, message) = self.reply().await?;
        if expect.contains(&code) {
            Ok(())
        } else {
            let message: String = message.chars().take(200).collect();
            Err(format!("邮件服务器拒绝了{label}：{code} {message}"))
        }
    }

    async fn deliver(&mut self, config: &MailConfig, message: &str) -> Result<(), String> {
        let b64 = base64::engine::general_purpose::STANDARD;
        self.command("EHLO codex-candy-eval", "问候", &[250]).await?;
        self.command("AUTH LOGIN", "登录", &[334]).await?;
        self.command(&b64.encode(&config.username), "登录账号", &[334])
            .await?;
        self.command(&b64.encode(&config.password), "登录（账号或授权码不对）", &[235])
            .await?;
        self.command(&format!("MAIL FROM:<{}>", config.username), "发件人", &[250])
            .await?;
        for address in &config.to {
            self.command(&format!("RCPT TO:<{address}>"), "收件人", &[250, 251])
                .await?;
        }
        self.command("DATA", "正文", &[354]).await?;
        self.command(&format!("{message}\r\n."), "正文", &[250]).await?;
        let _ = self.command("QUIT", "退出", &[221]).await;
        Ok(())
    }
}

fn encoded_word(text: &str) -> String {
    format!(
        "=?UTF-8?B?{}?=",
        base64::engine::general_purpose::STANDARD.encode(text)
    )
}

fn message(config: &MailConfig, subject: &str, body: &str) -> String {
    let b64 = base64::engine::general_purpose::STANDARD;
    let encoded = b64.encode(body.replace('\n', "\r\n"));
    let lines: Vec<&str> = encoded
        .as_bytes()
        .chunks(76)
        .map(|chunk| std::str::from_utf8(chunk).unwrap_or_default())
        .collect();
    let now = chrono::Utc::now();
    format!(
        "From: {} <{}>\r\nTo: {}\r\nSubject: {}\r\nDate: {}\r\nMessage-ID: <{}.{}@codex-candy-eval>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=UTF-8\r\nContent-Transfer-Encoding: base64\r\n\r\n{}",
        encoded_word("Codex 降智测试"),
        config.username,
        config.to.join(", "),
        encoded_word(subject),
        now.to_rfc2822(),
        now.timestamp_millis(),
        rand::random::<u32>(),
        lines.join("\r\n"),
    )
}

async fn send_inner(config: &MailConfig, subject: &str, body: &str) -> Result<(), String> {
    let message = message(config, subject, body);
    let server = ServerName::try_from(config.host.clone())
        .map_err(|_| "SMTP 服务器地址格式不对".to_owned())?;
    let tcp = TcpStream::connect((config.host.as_str(), config.port))
        .await
        .map_err(|_| format!("连不上 {}:{}", config.host, config.port))?;
    let connector = tls()?;
    if config.port == 465 {
        let tls = connector
            .connect(server, tcp)
            .await
            .map_err(|_| "和邮件服务器的 TLS 握手失败".to_owned())?;
        let mut conn = Conn { io: BufReader::new(tls) };
        conn.expect("连接", &[220]).await?;
        return conn.deliver(config, &message).await;
    }
    let mut plain = Conn { io: BufReader::new(tcp) };
    plain.expect("连接", &[220]).await?;
    plain.command("EHLO codex-candy-eval", "问候", &[250]).await?;
    plain.command("STARTTLS", "加密", &[220]).await?;
    let tls = connector
        .connect(server, plain.io.into_inner())
        .await
        .map_err(|_| "和邮件服务器的 TLS 握手失败".to_owned())?;
    Conn { io: BufReader::new(tls) }
        .deliver(config, &message)
        .await
}

/// 发一封纯文本邮件；整次会话 30 秒超时。
pub async fn send(config: &MailConfig, subject: &str, body: &str) -> Result<(), String> {
    tokio::time::timeout(TIMEOUT, send_inner(config, subject, body))
        .await
        .map_err(|_| "连接邮件服务器超时".to_owned())?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn complete() -> Value {
        json!({"email_enabled":true, "smtp_host":"smtp.qq.com", "smtp_port":"465",
            "smtp_username":"sender@qq.com", "smtp_password":"secret",
            "email_to":"a@example.com， b@example.com"})
    }

    #[test]
    fn disabled_or_incomplete_settings_are_explicit() {
        assert!(matches!(mail_config(&json!({})), Ok(None)));
        let config = mail_config(&complete()).unwrap().unwrap();
        assert_eq!(config.port, 465);
        assert_eq!(config.to, vec!["a@example.com", "b@example.com"]);
        for (field, value) in [
            ("smtp_password", json!("")),
            ("smtp_username", json!("not-an-address")),
            ("email_to", json!("x@example.com\r\nBcc: y@example.com")),
            ("smtp_port", json!("25")),
        ] {
            let mut data = complete();
            data[field] = value;
            assert!(mail_config(&data).is_err(), "{field}");
        }
    }

    #[test]
    fn message_headers_are_encoded_and_body_is_base64() {
        let config = mail_config(&complete()).unwrap().unwrap();
        let text = message(&config, "【降智告警】账号", "第一行\n第二行");
        assert!(text.contains("Subject: =?UTF-8?B?"));
        assert!(text.contains("To: a@example.com, b@example.com\r\n"));
        assert!(!text.contains("secret"));
        let body = text.split("\r\n\r\n").nth(1).unwrap().replace("\r\n", "");
        let decoded = base64::engine::general_purpose::STANDARD.decode(body).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "第一行\r\n第二行");
    }
}
