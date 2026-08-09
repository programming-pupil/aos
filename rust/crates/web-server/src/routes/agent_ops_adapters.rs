//! AgentOps adapter contracts and built-in registries.
//!
//! This module is intentionally small and dependency-light so open-source users
//! can copy the contract shape when adding capabilities, bot platforms, or
//! runtime backends without reading the AgentOps route implementation.

use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityContract {
    pub key: &'static str,
    pub display_name: &'static str,
    pub menu_key: &'static str,
    pub execution_mode: &'static str,
    pub supports_bot: bool,
    pub supports_watchdog: bool,
    pub required_permissions: &'static [&'static str],
    pub rollout: &'static str,
    pub description: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BotPlatformContract {
    pub key: &'static str,
    pub display_name: &'static str,
    pub inbound_modes: &'static [&'static str],
    pub supports_local_inbound: bool,
    pub supports_outbound: bool,
    pub requires_public_callback: bool,
    pub description: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeContract {
    pub key: &'static str,
    pub display_name: &'static str,
    pub isolation_mode: &'static str,
    pub supports_process_group_cancel: bool,
    pub supports_artifacts: bool,
    pub default_enabled: bool,
    pub description: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchDogActionContract {
    pub key: &'static str,
    pub display_name: &'static str,
    pub kind: &'static str,
    pub permission: &'static str,
    pub audit_action: &'static str,
    pub description: &'static str,
}

pub trait CapabilityAdapter: Send + Sync {
    fn key(&self) -> &'static str;
    fn contract(&self) -> CapabilityContract;
}

pub trait BotPlatformAdapter: Send + Sync {
    fn platform(&self) -> &'static str;
    fn contract(&self) -> BotPlatformContract;
}

pub trait TaskRuntimeAdapter: Send + Sync {
    fn mode(&self) -> &'static str;
    fn contract(&self) -> RuntimeContract;
}

pub trait WatchDogActionAdapter: Send + Sync {
    fn key(&self) -> &'static str;
    fn contract(&self) -> WatchDogActionContract;
}

#[derive(Debug, Clone)]
struct StaticCapabilityAdapter {
    contract: CapabilityContract,
}

impl CapabilityAdapter for StaticCapabilityAdapter {
    fn key(&self) -> &'static str {
        self.contract.key
    }

    fn contract(&self) -> CapabilityContract {
        self.contract.clone()
    }
}

#[derive(Debug, Default)]
pub struct CapabilityRegistry {
    adapters: BTreeMap<&'static str, StaticCapabilityAdapter>,
}

impl CapabilityRegistry {
    pub fn with_builtins() -> Self {
        let mut registry = Self::default();
        for contract in builtin_capability_contracts() {
            registry.register_static(contract);
        }
        registry
    }

    pub fn register_static(&mut self, contract: CapabilityContract) {
        let adapter = StaticCapabilityAdapter { contract };
        self.adapters.insert(adapter.key(), adapter);
    }

    pub fn contracts(&self) -> Vec<CapabilityContract> {
        let mut contracts = self
            .adapters
            .values()
            .map(CapabilityAdapter::contract)
            .collect::<Vec<_>>();
        contracts.sort_by_key(|contract| match contract.key {
            "ai_chat" => 0,
            "super_adversarial" => 1,
            "watchdog" => 2,
            "pm_assistant" => 3,
            "rd_agent" => 4,
            "nl2sql" => 5,
            "aos_router" => 6,
            "generic_ai" => 7,
            _ => 100,
        });
        contracts
    }
}

#[derive(Debug, Clone)]
struct StaticBotPlatformAdapter {
    contract: BotPlatformContract,
}

impl BotPlatformAdapter for StaticBotPlatformAdapter {
    fn platform(&self) -> &'static str {
        self.contract.key
    }

    fn contract(&self) -> BotPlatformContract {
        self.contract.clone()
    }
}

#[derive(Debug, Default)]
pub struct BotPlatformRegistry {
    adapters: BTreeMap<&'static str, StaticBotPlatformAdapter>,
}

impl BotPlatformRegistry {
    pub fn with_builtins() -> Self {
        let mut registry = Self::default();
        for contract in builtin_bot_platform_contracts() {
            registry.register_static(contract);
        }
        registry
    }

    pub fn register_static(&mut self, contract: BotPlatformContract) {
        let adapter = StaticBotPlatformAdapter { contract };
        self.adapters.insert(adapter.platform(), adapter);
    }

    pub fn contracts(&self) -> Vec<BotPlatformContract> {
        let mut contracts = self
            .adapters
            .values()
            .map(BotPlatformAdapter::contract)
            .collect::<Vec<_>>();
        contracts.sort_by_key(|contract| match contract.key {
            "dingtalk" => 0,
            "telegram" => 1,
            "feishu" => 2,
            "wecom" => 3,
            "slack" => 4,
            "discord" => 5,
            "whatsapp" => 6,
            "generic_webhook" => 7,
            _ => 100,
        });
        contracts
    }
}

#[derive(Debug, Clone)]
struct StaticRuntimeAdapter {
    contract: RuntimeContract,
}

impl TaskRuntimeAdapter for StaticRuntimeAdapter {
    fn mode(&self) -> &'static str {
        self.contract.key
    }

    fn contract(&self) -> RuntimeContract {
        self.contract.clone()
    }
}

#[derive(Debug, Default)]
pub struct RuntimeRegistry {
    adapters: BTreeMap<&'static str, StaticRuntimeAdapter>,
}

impl RuntimeRegistry {
    pub fn with_builtins() -> Self {
        let mut registry = Self::default();
        registry.register_static(RuntimeContract {
            key: "local_process",
            display_name: "Local Process",
            isolation_mode: "local_process",
            supports_process_group_cancel: true,
            supports_artifacts: true,
            default_enabled: true,
            description:
                "默认本机 workspace 隔离运行时，支持进程组取消、stdout/stderr 预览和 artifacts。",
        });
        registry.register_static(RuntimeContract {
            key: "docker_sandbox",
            display_name: "Docker Sandbox",
            isolation_mode: "docker_sandbox",
            supports_process_group_cancel: true,
            supports_artifacts: true,
            default_enabled: false,
            description:
                "可选容器沙箱运行时，通过 Docker 挂载任务 workspace 并在容器内执行命令；默认关闭。",
        });
        registry
    }

    pub fn register_static(&mut self, contract: RuntimeContract) {
        let adapter = StaticRuntimeAdapter { contract };
        self.adapters.insert(adapter.mode(), adapter);
    }

    pub fn contracts(&self) -> Vec<RuntimeContract> {
        self.adapters
            .values()
            .map(TaskRuntimeAdapter::contract)
            .collect()
    }
}

#[derive(Debug, Clone)]
struct StaticWatchDogActionAdapter {
    contract: WatchDogActionContract,
}

impl WatchDogActionAdapter for StaticWatchDogActionAdapter {
    fn key(&self) -> &'static str {
        self.contract.key
    }

    fn contract(&self) -> WatchDogActionContract {
        self.contract.clone()
    }
}

#[derive(Debug, Default)]
pub struct WatchDogActionRegistry {
    adapters: BTreeMap<&'static str, StaticWatchDogActionAdapter>,
}

impl WatchDogActionRegistry {
    pub fn with_builtins() -> Self {
        let mut registry = Self::default();
        for contract in builtin_watchdog_action_contracts() {
            registry.register_static(contract);
        }
        registry
    }

    pub fn register_static(&mut self, contract: WatchDogActionContract) {
        let adapter = StaticWatchDogActionAdapter { contract };
        self.adapters.insert(adapter.key(), adapter);
    }

    pub fn contracts(&self) -> Vec<WatchDogActionContract> {
        let mut contracts = self
            .adapters
            .values()
            .map(WatchDogActionAdapter::contract)
            .collect::<Vec<_>>();
        contracts.sort_by_key(|contract| match contract.key {
            "detail_task" => 0,
            "cancel_task" => 1,
            "retry_task" => 2,
            _ => 100,
        });
        contracts
    }

    pub fn contract_for_kind(&self, kind: &str) -> Option<WatchDogActionContract> {
        self.contracts()
            .into_iter()
            .find(|contract| contract.kind == kind)
    }
}

pub fn capability_contracts() -> Vec<CapabilityContract> {
    CapabilityRegistry::with_builtins().contracts()
}

pub fn bot_platform_contracts() -> Vec<BotPlatformContract> {
    BotPlatformRegistry::with_builtins().contracts()
}

pub fn runtime_contracts() -> Vec<RuntimeContract> {
    RuntimeRegistry::with_builtins().contracts()
}

pub fn watchdog_action_contracts() -> Vec<WatchDogActionContract> {
    WatchDogActionRegistry::with_builtins().contracts()
}

fn builtin_capability_contracts() -> Vec<CapabilityContract> {
    vec![
        CapabilityContract {
            key: "aos_router",
            display_name: "超级助手",
            menu_key: "super-assistant",
            execution_mode: "router",
            supports_bot: true,
            supports_watchdog: true,
            required_permissions: &["super_assistant:read", "bot_agents:read"],
            rollout: "P0",
            description: "统一复用 WebUI 超级助手的会话、上下文、任务和产物；任务查询与控制会在能力路由前处理。",
        },
        CapabilityContract {
            key: "rd_agent",
            display_name: "代码开发",
            menu_key: "agent",
            execution_mode: "async",
            supports_bot: true,
            supports_watchdog: true,
            required_permissions: &["rd_studio:read"],
            rollout: "P0",
            description: "创建真实 RD task，走 RD 执行器、上下文治理、diff、测试和审批。",
        },
        CapabilityContract {
            key: "nl2sql",
            display_name: "数据探索",
            menu_key: "nl2sql",
            execution_mode: "sync_or_clarify",
            supports_bot: true,
            supports_watchdog: true,
            required_permissions: &["nl2sql_explore:read"],
            rollout: "P1",
            description: "复用数据源、语义层、SQL 安全策略和执行权限；支持 Bot 同会话澄清式多轮。",
        },
        CapabilityContract {
            key: "super_adversarial",
            display_name: "超级对抗",
            menu_key: "adversarial",
            execution_mode: "async",
            supports_bot: true,
            supports_watchdog: true,
            required_permissions: &["adversarial:read"],
            rollout: "P0",
            description: "创建真实多模型对抗任务，任务状态、停止与完成通知进入统一任务闭环。",
        },
    ]
}

fn builtin_watchdog_action_contracts() -> Vec<WatchDogActionContract> {
    vec![
        WatchDogActionContract {
            key: "detail_task",
            display_name: "详情",
            kind: "detail",
            permission: "watchdog:read",
            audit_action: "open_task",
            description: "读取任务、trace、runtime 和最近事件详情，不改变任务状态。",
        },
        WatchDogActionContract {
            key: "cancel_task",
            display_name: "取消",
            kind: "cancel",
            permission: "watchdog:write",
            audit_action: "cancel_task",
            description: "请求取消任务；running runtime 会触发进程组或容器取消。",
        },
        WatchDogActionContract {
            key: "retry_task",
            display_name: "重试",
            kind: "retry",
            permission: "watchdog:write",
            audit_action: "retry_task",
            description: "基于原任务创建 child retry task，保留旧失败记录和审计事件。",
        },
    ]
}

fn builtin_bot_platform_contracts() -> Vec<BotPlatformContract> {
    vec![
        BotPlatformContract {
            key: "feishu",
            display_name: "飞书",
            inbound_modes: &["stream"],
            supports_local_inbound: true,
            supports_outbound: true,
            requires_public_callback: false,
            description: "支持本地长连接入站；出站支持自定义机器人 Webhook 或 OpenAPI。",
        },
        BotPlatformContract {
            key: "lark",
            display_name: "Lark",
            inbound_modes: &["stream"],
            supports_local_inbound: true,
            supports_outbound: true,
            requires_public_callback: false,
            description:
                "使用 Lark 国际版 API 和本地长连接；出站支持自定义机器人 Webhook 或 OpenAPI。",
        },
        BotPlatformContract {
            key: "wecom",
            display_name: "企业微信",
            inbound_modes: &["stream"],
            supports_local_inbound: true,
            supports_outbound: true,
            requires_public_callback: false,
            description: "支持 AI Bot WebSocket 本地入站；出站使用群机器人 Webhook。",
        },
        BotPlatformContract {
            key: "slack",
            display_name: "Slack",
            inbound_modes: &["socket"],
            supports_local_inbound: true,
            supports_outbound: true,
            requires_public_callback: false,
            description: "支持 Socket Mode 本地入站；出站支持 Incoming Webhook 或 Bot Token。",
        },
        BotPlatformContract {
            key: "discord",
            display_name: "Discord",
            inbound_modes: &["socket"],
            supports_local_inbound: true,
            supports_outbound: true,
            requires_public_callback: false,
            description: "支持 Gateway WebSocket 本地入站；出站支持 Webhook 或 Bot Token。",
        },
        BotPlatformContract {
            key: "dingtalk",
            display_name: "钉钉",
            inbound_modes: &["stream"],
            supports_local_inbound: true,
            supports_outbound: true,
            requires_public_callback: false,
            description: "支持 Stream 入站和群机器人 Webhook 出站。",
        },
        BotPlatformContract {
            key: "telegram",
            display_name: "Telegram",
            inbound_modes: &["polling", "webhook"],
            supports_local_inbound: true,
            supports_outbound: true,
            requires_public_callback: false,
            description: "支持 polling 本地入站和 sendMessage 出站。",
        },
        BotPlatformContract {
            key: "whatsapp",
            display_name: "WhatsApp",
            inbound_modes: &["webhook"],
            supports_local_inbound: false,
            supports_outbound: true,
            requires_public_callback: true,
            description: "Cloud API 官方入站需要公网 Webhook 或 relay。",
        },
        BotPlatformContract {
            key: "generic_webhook",
            display_name: "Generic Webhook",
            inbound_modes: &["webhook"],
            supports_local_inbound: false,
            supports_outbound: true,
            requires_public_callback: true,
            description: "用于自定义 relay、企业内部网关或第三方平台扩展。",
        },
    ]
}
