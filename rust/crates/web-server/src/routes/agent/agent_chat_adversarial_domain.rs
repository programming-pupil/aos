use super::*;

pub(super) const CHAT_ADV_HARD_MAX_ROUNDS: u32 = 50;

pub(super) const CHAT_ADV_CONSENSUS_VOTE_START: &str = "<aos_consensus_vote>";
pub(super) const CHAT_ADV_CONSENSUS_VOTE_END: &str = "</aos_consensus_vote>";
pub(super) const CHAT_ADV_EVIDENCE_REQUEST_START: &str = "<aos_evidence_request>";
pub(super) const CHAT_ADV_EVIDENCE_REQUEST_END: &str = "</aos_evidence_request>";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct EvidenceRequest {
    pub(super) needed: bool,
    pub(super) queries: Vec<String>,
    pub(super) reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ConsensusVote {
    pub(super) accept_consensus: bool,
    pub(super) preferred_winner_model: Option<String>,
    pub(super) remaining_objections: Vec<String>,
    pub(super) evidence_queries: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ParticipantConsensus {
    pub(super) reached: bool,
    pub(super) degraded_quorum: bool,
    pub(super) accepted_models: Vec<String>,
    pub(super) missing_or_rejected_models: Vec<String>,
    pub(super) remaining_objections: Vec<String>,
    pub(super) preferred_winner_models: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct ModelAnswer {
    pub(super) model: String,
    pub(super) answer: String,
    pub(super) error: Option<String>,
    pub(super) duration_ms: u128,
    pub(super) consensus_vote: Option<ConsensusVote>,
    pub(super) evidence_request: Option<EvidenceRequest>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct AdversarialDebateMemory {
    rounds: Vec<AdversarialDebateRoundMemory>,
}

#[derive(Debug, Clone)]
struct AdversarialDebateRoundMemory {
    round: u32,
    answers: Vec<ModelAnswer>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct JudgeDecision {
    pub(super) resolved: bool,
    pub(super) claim_audit_complete: bool,
    pub(super) critical_conflicts: Vec<String>,
    pub(super) winner_model: Option<String>,
    pub(super) winner_reason: Option<String>,
    pub(super) raw: String,
}

pub(super) fn build_initial_system_prompt(model: &str) -> String {
    crate::routes::builtin_skills::PromptRegistry::render(
        crate::routes::builtin_skills::PromptId::SuperAdversarialInitial,
        &[("model", model)],
    )
}

pub(super) fn build_review_system_prompt(model: &str, round: u32) -> String {
    let round = round.to_string();
    crate::routes::builtin_skills::PromptRegistry::render(
        crate::routes::builtin_skills::PromptId::SuperAdversarialReview,
        &[("model", model), ("round", &round)],
    )
}

pub(super) fn build_judge_system_prompt() -> String {
    crate::routes::builtin_skills::PromptRegistry::render(
        crate::routes::builtin_skills::PromptId::SuperAdversarialJudge,
        &[],
    )
}

pub(super) fn build_final_system_prompt() -> String {
    crate::routes::builtin_skills::PromptRegistry::render(
        crate::routes::builtin_skills::PromptId::SuperAdversarialFinal,
        &[],
    )
}

impl AdversarialDebateMemory {
    pub(super) fn record_round(&mut self, round: u32, answers: Vec<ModelAnswer>) {
        self.rounds
            .push(AdversarialDebateRoundMemory { round, answers });
        if self.rounds.len() > usize::try_from(CHAT_ADV_HARD_MAX_ROUNDS).unwrap_or(8) {
            let excess = self
                .rounds
                .len()
                .saturating_sub(usize::try_from(CHAT_ADV_HARD_MAX_ROUNDS).unwrap_or(8));
            self.rounds.drain(0..excess);
        }
    }

    pub(super) fn format_history_summary(
        &self,
        exclude_model: Option<&str>,
        max_chars: usize,
    ) -> String {
        let mut sections = Vec::new();
        for round in &self.rounds {
            let mut lines = Vec::new();
            for answer in &round.answers {
                if exclude_model.is_some_and(|model| answer.model.eq_ignore_ascii_case(model)) {
                    continue;
                }
                if let Some(error) = &answer.error {
                    lines.push(format!(
                        "- {}: 调用失败：{}",
                        answer.model,
                        truncate_chars(error, 240)
                    ));
                } else {
                    lines.push(format!(
                        "- {}: {}",
                        answer.model,
                        summarize_adversarial_answer_for_history(&answer.answer)
                    ));
                }
            }
            if !lines.is_empty() {
                sections.push(format!("### 第 {} 轮\n{}", round.round, lines.join("\n")));
            }
        }
        if sections.is_empty() {
            return "暂无可用历史观点轨迹。".to_string();
        }
        truncate_chars(&sections.join("\n\n"), max_chars)
    }
}

fn summarize_adversarial_answer_for_history(answer: &str) -> String {
    let normalized = answer
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(8)
        .collect::<Vec<_>>()
        .join(" ");
    truncate_chars(&normalized, 700)
}

pub(super) fn format_peer_answers(answers: &[ModelAnswer]) -> String {
    answers
        .iter()
        .map(|answer| {
            if let Some(error) = &answer.error {
                format!("## {}\n调用失败：{}\n", answer.model, error)
            } else {
                let vote = answer
                    .consensus_vote
                    .as_ref()
                    .map_or_else(String::new, |vote| {
                        let objections = if vote.remaining_objections.is_empty() {
                            "无".to_string()
                        } else {
                            vote.remaining_objections.join("；")
                        };
                        format!(
                            "\n\n一致认可票：{}；偏好胜出模型：{}；重大未决异议：{}；建议补证据查询：{}",
                            if vote.accept_consensus {
                                "认可"
                            } else {
                                "不认可"
                            },
                            vote.preferred_winner_model.as_deref().unwrap_or("未指定"),
                            objections,
                            if vote.evidence_queries.is_empty() {
                                "无".to_string()
                            } else {
                                vote.evidence_queries.join("；")
                            },
                        )
                    });
                format!("## {}\n{}{}\n", answer.model, answer.answer, vote)
            }
        })
        .collect::<Vec<_>>()
        .join("\n---\n")
}

pub(super) fn format_previous_judge_feedback(judge: &JudgeDecision) -> String {
    if judge.raw.trim().is_empty()
        && judge
            .winner_reason
            .as_deref()
            .is_none_or(|reason| reason.trim().is_empty())
    {
        return "上一轮尚未进入终局裁决。".to_string();
    }
    format!(
        "上一轮裁判是否确认收敛：{}\n裁判反馈：{}",
        if judge.resolved { "是" } else { "否" },
        judge.winner_reason.as_deref().unwrap_or("未提供具体反馈"),
    )
}

pub(super) fn format_peer_answers_for_reviewer(
    answers: &[ModelAnswer],
    reviewer_model: &str,
) -> String {
    let peer_answers = answers
        .iter()
        .filter(|answer| !answer.model.eq_ignore_ascii_case(reviewer_model))
        .cloned()
        .collect::<Vec<_>>();
    if peer_answers.is_empty() {
        return "未收到其他专家/模型的可用观点，请基于原问题独立修订。".to_string();
    }
    format_peer_answers(&peer_answers)
}

pub(super) fn format_own_previous_answer(answers: &[ModelAnswer], reviewer_model: &str) -> String {
    answers
        .iter()
        .find(|answer| answer.model.eq_ignore_ascii_case(reviewer_model))
        .map(|answer| {
            if let Some(error) = &answer.error {
                format!("你上一轮的调用失败：{error}")
            } else {
                answer.answer.clone()
            }
        })
        .filter(|answer| !answer.trim().is_empty())
        .unwrap_or_else(|| "没有可用的上一轮答案，请基于原问题和其他模型观点重新作答。".to_string())
}

pub(super) fn format_followup_context(parent_context: Option<&str>) -> String {
    let Some(context) = parent_context
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return String::new();
    };
    format!(
        "这是同一个超级对抗线程的追问。以下是此前对抗的关键上下文，请继承仍然有效的结论；如果用户新问题要求修正、补充或推翻旧结论，以新问题为准。\n\n{context}\n\n---\n\n"
    )
}

pub(super) fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut iter = value.chars();
    let truncated = iter.by_ref().take(max_chars).collect::<String>();
    if iter.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

pub(super) fn model_answer_to_json(answer: &ModelAnswer) -> serde_json::Value {
    serde_json::json!({
        "model": answer.model,
        "answer": answer.answer,
        "error": answer.error,
        "durationMs": answer.duration_ms,
        "consensusVote": answer.consensus_vote.as_ref().map(consensus_vote_to_json),
        "evidenceRequest": answer.evidence_request.as_ref().map(evidence_request_to_json),
    })
}

pub(super) fn participant_consensus_to_json(consensus: &ParticipantConsensus) -> serde_json::Value {
    serde_json::json!({
        "reached": consensus.reached,
        "degradedQuorum": consensus.degraded_quorum,
        "acceptedModels": consensus.accepted_models,
        "missingOrRejectedModels": consensus.missing_or_rejected_models,
        "remainingObjections": consensus.remaining_objections,
        "preferredWinnerModels": consensus.preferred_winner_models,
    })
}

fn consensus_vote_to_json(vote: &ConsensusVote) -> serde_json::Value {
    serde_json::json!({
        "acceptConsensus": vote.accept_consensus,
        "preferredWinnerModel": vote.preferred_winner_model,
        "remainingObjections": vote.remaining_objections,
        "evidenceQueries": vote.evidence_queries,
    })
}

fn evidence_request_to_json(request: &EvidenceRequest) -> serde_json::Value {
    serde_json::json!({
        "needed": request.needed,
        "queries": request.queries,
        "reason": request.reason,
    })
}

pub(super) fn parse_initial_answer(raw: &str) -> (String, Option<EvidenceRequest>) {
    let Some(start) = raw.rfind(CHAT_ADV_EVIDENCE_REQUEST_START) else {
        return (raw.trim().to_string(), None);
    };
    let request_start = start + CHAT_ADV_EVIDENCE_REQUEST_START.len();
    let Some(relative_end) = raw[request_start..].find(CHAT_ADV_EVIDENCE_REQUEST_END) else {
        return (raw.trim().to_string(), None);
    };
    let request_end = request_start + relative_end;
    let parsed =
        serde_json::from_str::<serde_json::Value>(raw[request_start..request_end].trim()).ok();
    let request = parsed.and_then(|value| {
        let needed = value.get("needed").and_then(serde_json::Value::as_bool)?;
        let queries = json_string_array(value.get("queries"), 3, 240);
        let reason = value
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .map(|reason| truncate_chars(reason, 400));
        Some(EvidenceRequest {
            needed,
            queries,
            reason,
        })
    });
    let end = request_end + CHAT_ADV_EVIDENCE_REQUEST_END.len();
    let cleaned = format!("{}{}", &raw[..start], &raw[end..])
        .trim()
        .to_string();
    (cleaned, request)
}

pub(super) fn parse_review_answer(raw: &str) -> (String, Option<ConsensusVote>) {
    let Some(start) = raw.rfind(CHAT_ADV_CONSENSUS_VOTE_START) else {
        return (raw.trim().to_string(), None);
    };
    let vote_start = start + CHAT_ADV_CONSENSUS_VOTE_START.len();
    let Some(relative_end) = raw[vote_start..].find(CHAT_ADV_CONSENSUS_VOTE_END) else {
        return (raw.trim().to_string(), None);
    };
    let vote_end = vote_start + relative_end;
    let parsed = serde_json::from_str::<serde_json::Value>(raw[vote_start..vote_end].trim()).ok();
    let vote = parsed.and_then(|value| {
        let accept_consensus = value
            .get("acceptConsensus")
            .or_else(|| value.get("accept_consensus"))
            .and_then(serde_json::Value::as_bool)?;
        let preferred_winner_model = value
            .get("preferredWinnerModel")
            .or_else(|| value.get("preferred_winner_model"))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(ToString::to_string);
        let remaining_objections = value
            .get("remainingObjections")
            .or_else(|| value.get("remaining_objections"))
            .and_then(serde_json::Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::trim)
                    .filter(|objection| !objection.is_empty())
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let evidence_queries = json_string_array(
            value
                .get("evidenceQueries")
                .or_else(|| value.get("evidence_queries")),
            3,
            240,
        );
        Some(ConsensusVote {
            accept_consensus,
            preferred_winner_model,
            remaining_objections,
            evidence_queries,
        })
    });
    let end = vote_end + CHAT_ADV_CONSENSUS_VOTE_END.len();
    let cleaned = format!("{}{}", &raw[..start], &raw[end..])
        .trim()
        .to_string();
    (cleaned, vote)
}

fn json_string_array(
    value: Option<&serde_json::Value>,
    limit: usize,
    max_chars: usize,
) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    value
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| truncate_chars(item, max_chars))
        .filter(|item| seen.insert(item.to_ascii_lowercase()))
        .take(limit)
        .collect()
}

pub(super) fn evaluate_participant_consensus(
    configured_models: &[String],
    answers: &[ModelAnswer],
) -> ParticipantConsensus {
    let mut consensus = ParticipantConsensus::default();
    for model in configured_models {
        let answer = answers
            .iter()
            .find(|answer| answer.model.eq_ignore_ascii_case(model));
        let Some(answer) =
            answer.filter(|answer| answer.error.is_none() && !answer.answer.trim().is_empty())
        else {
            consensus.missing_or_rejected_models.push(model.clone());
            continue;
        };
        let Some(vote) = answer.consensus_vote.as_ref() else {
            consensus.missing_or_rejected_models.push(model.clone());
            continue;
        };
        if !vote.accept_consensus || !vote.remaining_objections.is_empty() {
            consensus.missing_or_rejected_models.push(model.clone());
            consensus.remaining_objections.extend(
                vote.remaining_objections
                    .iter()
                    .map(|objection| format!("{model}: {objection}")),
            );
            continue;
        }
        consensus.accepted_models.push(model.clone());
        if let Some(winner) = vote.preferred_winner_model.as_deref().filter(|winner| {
            configured_models
                .iter()
                .any(|model| model.eq_ignore_ascii_case(winner))
        }) {
            consensus.preferred_winner_models.push(winner.to_string());
        }
    }
    let healthy_count = configured_models
        .len()
        .saturating_sub(consensus.missing_or_rejected_models.len());
    consensus.degraded_quorum = healthy_count >= 2 && healthy_count < configured_models.len();
    consensus.reached = healthy_count >= 2
        && consensus.accepted_models.len() == healthy_count
        && consensus.remaining_objections.is_empty();
    consensus
}

pub(super) fn is_configured_winner(winner: Option<&str>, configured_models: &[String]) -> bool {
    winner.is_some_and(|winner| {
        configured_models
            .iter()
            .any(|model| model.eq_ignore_ascii_case(winner))
    })
}

pub(super) fn preferred_consensus_winner(
    consensus: &ParticipantConsensus,
    configured_models: &[String],
) -> Option<String> {
    configured_models
        .iter()
        .map(|model| {
            let votes = consensus
                .preferred_winner_models
                .iter()
                .filter(|winner| winner.eq_ignore_ascii_case(model))
                .count();
            (model, votes)
        })
        .filter(|(_, votes)| *votes > 0)
        .max_by_key(|(_, votes)| *votes)
        .map(|(model, _)| model.clone())
}

pub(super) fn parse_judge_decision(raw: &str) -> JudgeDecision {
    let value = extract_first_json_object(raw)
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok());
    let Some(value) = value else {
        return JudgeDecision {
            resolved: false,
            claim_audit_complete: false,
            critical_conflicts: vec!["judge response was not valid JSON".to_string()],
            winner_model: None,
            winner_reason: Some("judge response was not valid JSON".to_string()),
            raw: raw.to_string(),
        };
    };
    JudgeDecision {
        resolved: value
            .get("resolved")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        claim_audit_complete: value
            .get("claim_audit_complete")
            .or_else(|| value.get("claimAuditComplete"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        critical_conflicts: value
            .get("critical_conflicts")
            .or_else(|| value.get("criticalConflicts"))
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .take(12)
            .collect(),
        winner_model: value
            .get("winner_model")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
        winner_reason: value
            .get("winner_reason")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
        raw: raw.to_string(),
    }
}

pub(super) fn parse_final_decision(raw: &str) -> JudgeDecision {
    let value = extract_first_json_object(raw)
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok());
    let Some(value) = value else {
        return JudgeDecision {
            resolved: true,
            claim_audit_complete: true,
            critical_conflicts: Vec::new(),
            winner_model: None,
            winner_reason: Some("final response was not valid JSON".to_string()),
            raw: raw.to_string(),
        };
    };
    JudgeDecision {
        resolved: true,
        claim_audit_complete: true,
        critical_conflicts: Vec::new(),
        winner_model: value
            .get("winner_model")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
        winner_reason: value
            .get("winner_reason")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
        raw: value
            .get("final_answer")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(raw)
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(model: &str, text: &str) -> ModelAnswer {
        ModelAnswer {
            model: model.to_string(),
            answer: text.to_string(),
            error: None,
            duration_ms: 1,
            consensus_vote: None,
            evidence_request: None,
        }
    }

    #[test]
    fn directed_review_context_excludes_reviewer_own_previous_answer() {
        let answers = vec![
            answer("A", "A previous answer"),
            answer("B", "B says market risk is the key issue"),
            answer("C", "C says compliance evidence is missing"),
        ];

        let context = format_peer_answers_for_reviewer(&answers, "A");

        assert!(!context.contains("A previous answer"));
        assert!(context.contains("## B"));
        assert!(context.contains("B says market risk is the key issue"));
        assert!(context.contains("## C"));
        assert!(context.contains("C says compliance evidence is missing"));
        assert_eq!(
            format_own_previous_answer(&answers, "A"),
            "A previous answer"
        );
    }

    #[test]
    fn review_vote_is_removed_from_visible_answer_and_parsed() {
        let raw = format!(
            "Revised answer.\n{}{{\"acceptConsensus\":true,\"preferredWinnerModel\":\"B\",\"remainingObjections\":[]}}{}",
            CHAT_ADV_CONSENSUS_VOTE_START, CHAT_ADV_CONSENSUS_VOTE_END
        );
        let (answer, vote) = parse_review_answer(&raw);
        assert_eq!(answer, "Revised answer.");
        assert_eq!(
            vote.unwrap(),
            ConsensusVote {
                accept_consensus: true,
                preferred_winner_model: Some("B".to_string()),
                remaining_objections: Vec::new(),
                evidence_queries: Vec::new(),
            }
        );
    }

    #[test]
    fn peer_context_keeps_structured_objections_and_judge_feedback() {
        let mut peer = answer("B", "The evidence is incomplete.");
        peer.consensus_vote = Some(ConsensusVote {
            accept_consensus: false,
            preferred_winner_model: None,
            remaining_objections: vec!["缺少成本数据".to_string()],
            evidence_queries: vec!["项目成本基准".to_string()],
        });
        let context = format_peer_answers(&[peer]);
        assert!(context.contains("一致认可票：不认可"));
        assert!(context.contains("缺少成本数据"));
        assert!(context.contains("项目成本基准"));

        let judge = JudgeDecision {
            resolved: false,
            claim_audit_complete: false,
            critical_conflicts: vec!["关键来源相互冲突".to_string()],
            winner_model: None,
            winner_reason: Some("关键来源相互冲突".to_string()),
            raw: r#"{"resolved":false}"#.to_string(),
        };
        assert!(format_previous_judge_feedback(&judge).contains("关键来源相互冲突"));
    }

    #[test]
    fn consensus_requires_every_configured_model_to_accept_without_objections() {
        let models = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let mut answers = models
            .iter()
            .map(|model| answer(model, "revised"))
            .collect::<Vec<_>>();
        for answer in &mut answers {
            answer.consensus_vote = Some(ConsensusVote {
                accept_consensus: true,
                preferred_winner_model: Some("B".to_string()),
                remaining_objections: Vec::new(),
                evidence_queries: Vec::new(),
            });
        }
        let reached = evaluate_participant_consensus(&models, &answers);
        assert!(reached.reached);
        assert_eq!(
            preferred_consensus_winner(&reached, &models).as_deref(),
            Some("B")
        );

        answers[2].consensus_vote = Some(ConsensusVote {
            accept_consensus: false,
            preferred_winner_model: None,
            remaining_objections: vec!["关键事实仍未验证".to_string()],
            evidence_queries: vec!["关键事实的权威来源".to_string()],
        });
        let rejected = evaluate_participant_consensus(&models, &answers);
        assert!(!rejected.reached);
        assert_eq!(rejected.missing_or_rejected_models, vec!["C"]);
        assert!(rejected.remaining_objections[0].contains("关键事实仍未验证"));
    }

    #[test]
    fn initial_evidence_marker_is_parsed_and_removed_from_visible_answer() {
        let raw = format!(
            "Architecture answer.\n{}{{\"needed\":true,\"queries\":[\"official benchmark\"],\"reason\":\"version-sensitive\"}}{}",
            CHAT_ADV_EVIDENCE_REQUEST_START, CHAT_ADV_EVIDENCE_REQUEST_END
        );
        let (answer, request) = parse_initial_answer(&raw);
        assert_eq!(answer, "Architecture answer.");
        assert_eq!(
            request.unwrap(),
            EvidenceRequest {
                needed: true,
                queries: vec!["official benchmark".to_string()],
                reason: Some("version-sensitive".to_string()),
            }
        );
    }

    #[test]
    fn debate_memory_history_summary_excludes_reviewer_but_keeps_peer_trajectory() {
        let mut memory = AdversarialDebateMemory::default();
        memory.record_round(
            1,
            vec![
                answer("A", "A round 1 answer"),
                answer("B", "B round 1: market risk"),
                answer("C", "C round 1: compliance gap"),
            ],
        );
        memory.record_round(
            2,
            vec![
                answer("A", "A round 2 answer"),
                answer("B", "B round 2: risk narrowed to payment channels"),
                answer("C", "C round 2: compliance evidence improved"),
            ],
        );
        memory.record_round(
            3,
            vec![
                answer("A", "A round 3 answer"),
                answer("B", "B round 3: payment channel risk remains"),
                answer("C", "C round 3: compliance issue mostly resolved"),
            ],
        );

        let history = memory.format_history_summary(Some("A"), 4000);

        assert!(!history.contains("A round"));
        assert!(history.contains("第 1 轮"));
        assert!(history.contains("B round 1: market risk"));
        assert!(history.contains("C round 1: compliance gap"));
        assert!(history.contains("第 2 轮"));
        assert!(history.contains("B round 2: risk narrowed to payment channels"));
        assert!(history.contains("C round 2: compliance evidence improved"));
        assert!(history.contains("第 3 轮"));
        assert!(history.contains("B round 3: payment channel risk remains"));
        assert!(history.contains("C round 3: compliance issue mostly resolved"));
    }

    #[test]
    fn skill_prompts_preserve_adversarial_contract() {
        let initial = build_initial_system_prompt("A");
        assert!(initial.contains("参赛模型 A"));
        assert!(initial.contains("事实优先"));

        let review = build_review_system_prompt("B", 3);
        assert!(review.contains("当前第 3 轮"));
        assert!(review.contains("自己的上一轮完整答案"));
        assert!(review.contains("一致认可票"));

        let judge = build_judge_system_prompt();
        assert!(judge.contains("resolved=true"));

        let final_prompt = build_final_system_prompt();
        assert!(final_prompt.contains("最终答案整理者"));
    }
}
