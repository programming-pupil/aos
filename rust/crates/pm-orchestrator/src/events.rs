#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmStageStatus {
    Running,
    Completed,
    Failed,
}

impl PmStageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

pub fn pm_stage_user_message(stage: &str, status: &str) -> Option<&'static str> {
    match (stage, status) {
        ("preflight", "running") => Some("正在执行启动健康检查"),
        ("preflight", "completed") => Some("启动健康检查通过"),
        ("preflight", "failed") => Some("启动健康检查失败"),
        ("understand", "running") => Some("正在理解问题与研究目标"),
        ("understand", "completed") => Some("问题理解完成"),
        ("understand", "failed") => Some("问题理解失败，切换到降级方案"),
        ("task_plan", "running") => Some("正在生成任务规划"),
        ("task_plan", "completed") => Some("任务规划已生成"),
        ("task_plan", "failed") => Some("任务规划失败，继续执行兜底流程"),
        ("planner", "running") => Some("正在规划检索策略"),
        ("planner", "completed") => Some("已生成检索计划"),
        ("retrieve", "running") => Some("正在跨来源检索与抓取证据"),
        ("retrieve", "completed") => Some("多源检索完成"),
        ("retrieve", "failed") => Some("多源检索失败，准备自动修复"),
        ("verify", "running") => Some("正在校验证据一致性"),
        ("verify", "completed") => Some("证据校验完成"),
        ("verify", "failed") => Some("证据校验失败，准备修复"),
        ("retry_repair", "running") => Some("正在自动修复质量问题"),
        ("retry_repair", "completed") => Some("自动修复完成"),
        ("retry_repair", "failed") => Some("自动修复失败，准备降级输出"),
        ("synthesize", "running") => Some("正在整合最终结论"),
        ("synthesize", "completed") => Some("已完成结论汇总"),
        ("synthesize", "failed") => Some("结论已生成，但质量偏低"),
        _ => None,
    }
}
