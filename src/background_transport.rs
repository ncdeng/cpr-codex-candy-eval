//! Use one administrator-selected CPR Client Key, then release the SDK scope before execution.
//! Without a selected Key, each account gets an instance-owned single-account group and Key,
//! so a test request can only be answered by the account it is recorded under.
use crate::{
    execution::{self, HttpExecutor},
    model::Credential,
};
use gateway_plugin_sdk::call::{
    middleware::http::Version,
    resources::{GroupEnsureRequest, GroupMembersChange, KeyEnsureRequest},
};
use gateway_plugin_sdk::{
    client::{HostClient, HttpBody, HttpFrame, HttpRequest},
    ErrorCode, PluginFault,
};
use reqwest::Url;
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::Value;

fn invalid(message: &str) -> PluginFault {
    PluginFault::new(ErrorCode::Conflict, message)
}
#[derive(Deserialize)]
struct Envelope<T> {
    data: T,
}
#[derive(Deserialize)]
struct GroupRef {
    enabled: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct KeyInfo {
    id: String,
    #[serde(default)]
    name: String,
    enabled: bool,
    #[serde(default)]
    groups: Vec<GroupRef>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Keys {
    items: Vec<KeyInfo>,
    next_cursor: Option<String>,
}
#[derive(Deserialize)]
struct Member {
    id: String,
}
#[derive(Deserialize)]
struct Members {
    items: Vec<Member>,
    page: MemberPage,
}
#[derive(Deserialize)]
struct MemberPage {
    total: u64,
}
#[derive(Deserialize)]
struct OwnedGroupRef {
    id: String,
    enabled: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OwnedKeyInfo {
    id: String,
    enabled: bool,
    groups: Vec<OwnedGroupRef>,
    max_concurrency: u64,
    requests_per_minute: u64,
    daily_limit_usd: String,
    weekly_limit_usd: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OwnedKeys {
    items: Vec<OwnedKeyInfo>,
    next_cursor: Option<String>,
}
// Never implement Debug/Serialize on the reveal response.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Revealed {
    id: String,
    plaintext_key: String,
}

pub(crate) async fn admin<T: DeserializeOwned>(host: &HostClient, path: String) -> Result<T, PluginFault> {
    let mut response = host
        .dispatch_http(HttpRequest {
            settings: Value::Null,
            method: "GET".into(),
            uri: path,
            version: Version::Http11,
            headers: vec![],
            timeout_ms: Some(10_000),
            body: HttpBody::empty(),
        })
        .await
        .map_err(|_| invalid("CPR 原生管理接口调用失败"))?;
    if response.status != 200 {
        return Err(invalid("CPR 原生管理接口拒绝请求"));
    }
    let mut bytes = Vec::new();
    while let Some(frame) = response.body.read().await? {
        if let HttpFrame::Data(data) = frame {
            if bytes.len().saturating_add(data.len()) > 512 * 1024 {
                return Err(invalid("CPR 管理响应超过大小上限"));
            }
            bytes.extend_from_slice(&data);
        }
    }
    serde_json::from_slice::<Envelope<T>>(&bytes)
        .map(|v| v.data)
        .map_err(|_| invalid("CPR 原生管理响应格式不兼容"))
}
pub(crate) fn query(path: &str, params: &[(&str, &str)]) -> String {
    let parameters =
        reqwest::Url::parse_with_params("http://127.0.0.1/", params).expect("fixed URL");
    format!("{path}?{}", parameters.query().unwrap_or_default())
}
fn configured_selector(config: &Value) -> Result<(&str, &str), PluginFault> {
    let id = config
        .get("test_key_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    let name = config
        .get("test_key_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_default();
    if id.is_empty() && name.is_empty() {
        return Err(invalid(
            "请在插件配置中填写已有测试 Key ID 或名称；插件不会自动创建分组和 Key",
        ));
    }
    if !id.is_empty() && !name.is_empty() {
        return Err(invalid("测试 Key ID 和名称只能填写一个"));
    }
    Ok((id, name))
}

async fn find_key(host: &HostClient, config: &Value) -> Result<KeyInfo, PluginFault> {
    let (wanted_id, wanted_name) = configured_selector(config)?;
    let mut cursor = String::new();
    let mut matches = Vec::new();
    loop {
        let mut params = vec![("limit", "100")];
        if !cursor.is_empty() {
            params.push(("cursor", cursor.as_str()));
        }
        let page: Keys = admin(host, query("/api/admin/client-keys", &params)).await?;
        matches.extend(page.items.into_iter().filter(|key| {
            (!wanted_id.is_empty() && key.id == wanted_id)
                || (!wanted_name.is_empty() && key.name == wanted_name)
        }));
        let Some(next) = page.next_cursor.filter(|next| next != &cursor) else {
            break;
        };
        cursor = next;
    }
    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => Err(invalid("找不到配置的测试 Key，请检查 Key ID 或名称")),
        _ => Err(invalid("测试 Key 名称不唯一，请改用 Key ID")),
    }
}

/// Either field filled selects the shared-Key mode; both empty selects per-account owned Keys.
fn has_key_selector(config: &Value) -> bool {
    ["test_key_id", "test_key_name"].iter().any(|field| {
        config
            .get(field)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    })
}

fn owned_resource_key(credential: &Credential) -> String {
    format!("candy-eval-{}", credential.id)
}

fn owned_resource_name(credential: &Credential) -> String {
    let label = [&credential.email, &credential.name, &credential.id]
        .into_iter()
        .map(|value| value.trim())
        .find(|value| !value.is_empty())
        .unwrap_or_default();
    // 账号删除后重新登录会换 ID；名称带上 ID 末 6 位，避免和旧账号留下的同名分组冲突。
    let id = &credential.id;
    let suffix = id.get(id.len().saturating_sub(6)..).unwrap_or(id);
    format!("降智测试·{label}·{suffix}")
}

fn verify_members(members: &Members, account: &str) -> Result<(), PluginFault> {
    if members.page.total != 1 || members.items.len() != 1 || members.items[0].id != account {
        return Err(invalid(
            "插件自有测试分组必须且只能包含目标账号；已拒绝执行，请在 CPR 分组管理中移除多余账号",
        ));
    }
    Ok(())
}

fn verify_owned_key(key: &OwnedKeyInfo, group: &str) -> Result<(), PluginFault> {
    let positive = |s: &str| s.parse::<f64>().is_ok_and(|n| n.is_finite() && n > 0.0);
    if !key.enabled
        || key.groups.len() != 1
        || key.groups[0].id != group
        || !key.groups[0].enabled
        || !(1..=16).contains(&key.max_concurrency)
        || !(1..=120).contains(&key.requests_per_minute)
        || !positive(&key.daily_limit_usd)
        || !positive(&key.weekly_limit_usd)
    {
        return Err(invalid(
            "插件自有测试 Key 的分组、启用状态或限额被改成了不安全的值；已拒绝执行",
        ));
    }
    Ok(())
}

/// Ensure the account's own group and Key, check nobody widened them, then reveal the Key.
async fn owned_key(host: &HostClient, credential: &Credential) -> Result<String, PluginFault> {
    let resource_key = owned_resource_key(credential);
    let name = owned_resource_name(credential);
    let group = host
        .ensure_group(GroupEnsureRequest {
            resource_key: resource_key.clone(),
            name: name.clone(),
            color: "#D97706FF".into(),
            description: Some(
                "降智测试插件自有的单账号分组，保证测试只由这个账号回答；不影响账号原有分组。".into(),
            ),
        })
        .await?;
    if !group.enabled {
        return Err(invalid("插件自有测试分组已停用"));
    }
    host.change_group_members(GroupMembersChange {
        resource_key: resource_key.clone(),
        add: vec![credential.id.clone()],
        remove: vec![],
    })
    .await?;
    let members: Members = admin(
        host,
        query(
            "/api/admin/accounts",
            &[("groupId", &group.id), ("pageSize", "2")],
        ),
    )
    .await?;
    verify_members(&members, &credential.id)?;
    let key = host
        .ensure_key(KeyEnsureRequest {
            resource_key: resource_key.clone(),
            name,
            group_resource_keys: vec![resource_key],
            max_concurrency: 16,
            requests_per_minute: 120,
            daily_limit_usd: "10".into(),
            weekly_limit_usd: "50".into(),
        })
        .await?;
    if !key.enabled {
        return Err(invalid("插件自有测试 Key 已停用"));
    }
    let mut cursor = String::new();
    loop {
        let mut params = vec![("search", key.name.as_str()), ("limit", "100")];
        if !cursor.is_empty() {
            params.push(("cursor", cursor.as_str()));
        }
        let page: OwnedKeys = admin(host, query("/api/admin/client-keys", &params)).await?;
        if let Some(info) = page.items.iter().find(|info| info.id == key.id) {
            verify_owned_key(info, &group.id)?;
            break;
        }
        let Some(next) = page.next_cursor.filter(|next| next != &cursor) else {
            return Err(invalid("无法核验插件自有测试 Key 的权限"));
        };
        cursor = next;
    }
    let secret: Revealed = admin(
        host,
        query("/api/admin/client-keys/reveal", &[("id", &key.id)]),
    )
    .await?;
    if secret.id != key.id {
        return Err(invalid("插件自有测试 Key 身份不一致"));
    }
    Ok(secret.plaintext_key)
}

/// Resolve and reveal the configured Key once for a whole background batch,
/// or leave it empty to provision one owned Key per account.
pub struct PreparedTransport {
    endpoint: Url,
    shared_key: Option<String>,
}

impl PreparedTransport {
    pub async fn for_credential(
        &self,
        host: &HostClient,
        credential: &Credential,
    ) -> Result<HttpExecutor, PluginFault> {
        let plaintext_key = match &self.shared_key {
            Some(key) => key.clone(),
            None => owned_key(host, credential).await?,
        };
        HttpExecutor::new(
            self.endpoint.clone(),
            credential.id.clone(),
            credential.provider.clone(),
            plaintext_key,
        )
    }
}

pub async fn prepare_shared(
    host: &HostClient,
    config: &Value,
) -> Result<PreparedTransport, PluginFault> {
    let endpoint = execution::endpoint(
        config
            .get("api_base_url")
            .and_then(Value::as_str)
            .unwrap_or("http://127.0.0.1:8080"),
    )?;
    if !has_key_selector(config) {
        return Ok(PreparedTransport {
            endpoint,
            shared_key: None,
        });
    }
    let key = find_key(host, config).await?;
    if !key.enabled {
        return Err(invalid("配置的测试 Key 已停用"));
    }
    if key.groups.is_empty() || key.groups.iter().any(|group| !group.enabled) {
        return Err(invalid(
            "配置的测试 Key 未绑定有效分组，请检查其账号访问范围",
        ));
    }
    let secret: Revealed = admin(
        host,
        query("/api/admin/client-keys/reveal", &[("id", &key.id)]),
    )
    .await?;
    if secret.id != key.id {
        return Err(invalid("配置的测试 Key 身份不一致"));
    }
    Ok(PreparedTransport {
        endpoint,
        shared_key: Some(secret.plaintext_key),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn requires_one_existing_key_selector() {
        assert_eq!(
            configured_selector(&json!({"test_key_id":" key_1 "})).unwrap(),
            ("key_1", "")
        );
        assert_eq!(
            configured_selector(&json!({"test_key_name":" shared test "})).unwrap(),
            ("", "shared test")
        );
        assert!(configured_selector(&json!({})).is_err());
        assert!(
            configured_selector(&json!({"test_key_id":"key_1", "test_key_name":"shared"})).is_err()
        );
    }
    #[test]
    fn key_info_accepts_admin_list_shape() {
        let key: KeyInfo = serde_json::from_value(json!({
            "id":"key_1", "name":"shared test", "enabled":true,
            "groups":[{"id":"all", "enabled":true}]
        }))
        .unwrap();
        assert_eq!(key.id, "key_1");
        assert_eq!(key.name, "shared test");
        assert!(key.enabled);
    }
    #[test]
    fn empty_selector_switches_to_owned_keys() {
        assert!(!has_key_selector(&json!({})));
        assert!(!has_key_selector(&json!({"test_key_id":"  ", "test_key_name":""})));
        assert!(has_key_selector(&json!({"test_key_id":"key_1"})));
        assert!(has_key_selector(&json!({"test_key_name":"shared"})));
    }
    #[test]
    fn owned_resources_use_valid_keys_and_readable_names() {
        let credential: Credential = serde_json::from_value(json!({
            "id":"acct_01a18d0e3a59739e8cf13a342e01e7d0", "provider":"openai",
            "source":"cpr", "name":"luo", "email":"luo@example.com"
        }))
        .unwrap();
        let key = owned_resource_key(&credential);
        assert!(key.len() <= 64);
        assert!(key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_.-".contains(c)));
        assert_eq!(owned_resource_name(&credential), "降智测试·luo@example.com·01e7d0");
    }
    #[test]
    fn owned_key_rejects_widened_scope() {
        let baseline = json!({"id":"key_1", "enabled":true, "groups":[{"id":"grp_1","enabled":true}],
            "maxConcurrency":16, "requestsPerMinute":120, "dailyLimitUsd":"10", "weeklyLimitUsd":"50"});
        let key: OwnedKeyInfo = serde_json::from_value(baseline.clone()).unwrap();
        assert!(verify_owned_key(&key, "grp_1").is_ok());
        for (field, value) in [
            ("groups", json!([])),
            ("groups", json!([{"id":"grp_1","enabled":true},{"id":"grp_2","enabled":true}])),
            ("enabled", json!(false)),
            ("maxConcurrency", json!(0)),
            ("dailyLimitUsd", json!("0")),
        ] {
            let mut data = baseline.clone();
            data[field] = value;
            let key: OwnedKeyInfo = serde_json::from_value(data).unwrap();
            assert!(verify_owned_key(&key, "grp_1").is_err(), "{field}");
        }
        let members: Members = serde_json::from_value(
            json!({"items":[{"id":"acct_a"},{"id":"acct_b"}], "page":{"total":2}}),
        )
        .unwrap();
        assert!(verify_members(&members, "acct_a").is_err());
    }
}
