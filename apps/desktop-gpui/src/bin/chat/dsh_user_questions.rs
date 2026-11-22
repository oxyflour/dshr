use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use dshr_compat_dsh_host::{
    HostService, HostServiceKind, HostServiceMethod, HostServiceTable, ServiceCallContext,
    ServiceError, USER_QUESTIONS_V1,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const MAX_PENDING: usize = 16;
const MAX_CUSTOM_BYTES: usize = 1_024;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Question {
    pub id: String,
    pub question: String,
    pub header: Option<String>,
    pub options: Vec<Choice>,
    pub multi_select: bool,
}

#[derive(Clone, Deserialize)]
pub(super) struct Choice {
    pub label: String,
    pub description: Option<String>,
}

#[derive(Serialize)]
struct Answer {
    id: String,
    selected: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    custom: Option<String>,
}

#[derive(Clone)]
pub(super) struct Prompt {
    pub ticket: u64,
    pub questions: Vec<Question>,
}

struct Pending {
    session: String,
    prompt: Prompt,
    response: Option<Result<Vec<Answer>, ()>>,
}

#[derive(Default)]
struct PendingQuestions {
    next_ticket: u64,
    prompts: VecDeque<Pending>,
}

#[derive(Default)]
pub(super) struct QuestionBroker {
    pending: Mutex<PendingQuestions>,
}

impl QuestionBroker {
    fn pending(&self) -> MutexGuard<'_, PendingQuestions> {
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    fn begin(&self, session: &str, questions: Vec<Question>) -> Result<u64, ServiceError> {
        let mut pending = self.pending();
        if pending.prompts.len() >= MAX_PENDING {
            return Err(ServiceError::new(
                "E_SERVICE_BUSY",
                "too many pending user questions",
            ));
        }
        pending.next_ticket += 1;
        let ticket = pending.next_ticket;
        pending.prompts.push_back(Pending {
            session: session.to_owned(),
            prompt: Prompt { ticket, questions },
            response: None,
        });
        Ok(ticket)
    }

    pub(super) fn first(&self, session: &str, displayed: Option<u64>) -> Option<Prompt> {
        let pending = self.pending();
        let prompt = pending
            .prompts
            .iter()
            .find(|entry| entry.session == session && entry.response.is_none())?;
        (Some(prompt.prompt.ticket) != displayed).then(|| prompt.prompt.clone())
    }

    pub(super) fn showing(&self, session: &str, ticket: u64) -> bool {
        self.pending().prompts.iter().any(|entry| {
            entry.session == session && entry.prompt.ticket == ticket && entry.response.is_none()
        })
    }

    pub(super) fn resolve(
        &self,
        session: &str,
        ticket: u64,
        answers: Option<Vec<(Vec<String>, String)>>,
    ) -> Result<(), &'static str> {
        let mut pending = self.pending();
        let entry = pending
            .prompts
            .iter_mut()
            .find(|entry| {
                entry.session == session
                    && entry.prompt.ticket == ticket
                    && entry.response.is_none()
            })
            .ok_or("此提问已结束")?;
        let Some(answers) = answers else {
            entry.response = Some(Err(()));
            return Ok(());
        };
        if answers.len() != entry.prompt.questions.len() {
            return Err("答复数量与问题数量不符");
        }
        let validated = entry
            .prompt
            .questions
            .iter()
            .zip(answers)
            .map(|(question, (selected, custom))| {
                let custom = custom.trim();
                if custom.len() > MAX_CUSTOM_BYTES {
                    return Err("文字答复过长");
                }
                let unique: HashSet<_> = selected.iter().collect();
                if (!question.multi_select && selected.len() > 1)
                    || unique.len() != selected.len()
                    || selected
                        .iter()
                        .any(|label| !question.options.iter().any(|option| &option.label == label))
                {
                    return Err("答复包含无效选项");
                }
                Ok(Answer {
                    id: question.id.clone(),
                    selected,
                    custom: (!custom.is_empty()).then(|| custom.to_owned()),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        entry.response = Some(Ok(validated));
        Ok(())
    }

    fn poll(&self, session: &str, ticket: u64) -> Result<Value, ServiceError> {
        let pending = self.pending();
        let entry = pending
            .prompts
            .iter()
            .find(|entry| entry.session == session && entry.prompt.ticket == ticket)
            .ok_or_else(|| ServiceError::new("E_CANCELLED", "user question is no longer active"))?;
        match &entry.response {
            None => Ok(json!({ "done": false })),
            Some(Ok(answers)) => Ok(json!({ "done": true, "answers": answers })),
            Some(Err(())) => Err(ServiceError::new(
                "E_CANCELLED",
                "user cancelled the question",
            )),
        }
    }

    fn release(&self, ticket: u64) {
        self.pending()
            .prompts
            .retain(|entry| entry.prompt.ticket != ticket);
    }
}

pub(super) struct UserQuestionService {
    table: HostServiceTable,
    broker: Option<Arc<QuestionBroker>>,
    tickets: HashMap<String, u64>,
}

impl UserQuestionService {
    pub(super) fn new(broker: Option<Arc<QuestionBroker>>) -> Self {
        let text = |limit| json!({ "type": "string", "minLength": 1, "maxLength": limit });
        let object = |required: &[&str], properties| {
            json!({
                "type": "object", "required": required, "properties": properties,
                "additionalProperties": false,
            })
        };
        let method = |name: &str, kind, idempotent, args_schema, result_schema| HostServiceMethod {
            name: name.to_owned(),
            kind,
            timeout_ms: 5_000,
            idempotent,
            requires_session: true,
            args_schema,
            result_schema,
        };
        let choices = object(
            &["label"],
            json!({ "label": text(80), "description": text(240) }),
        );
        let question = object(
            &["id", "question", "options", "multiSelect"],
            json!({
                "id": text(64), "question": text(1_024), "header": text(80),
                "options": { "type": "array", "maxItems": 8, "items": choices },
                "multiSelect": { "type": "boolean" },
            }),
        );
        let answers = object(
            &["id", "selected"],
            json!({
                "id": text(64),
                "selected": { "type": "array", "maxItems": 8, "items": text(80) },
                "custom": text(MAX_CUSTOM_BYTES),
            }),
        );
        let begin = method(
            "begin",
            HostServiceKind::Write,
            false,
            object(
                &["questions"],
                json!({
                    "questions": { "type": "array", "minItems": 1, "maxItems": 5, "items": question },
                }),
            ),
            object(&[], json!({})),
        );
        let poll = method(
            "poll",
            HostServiceKind::Read,
            true,
            object(&[], json!({})),
            object(
                &["done"],
                json!({
                    "done": { "type": "boolean" },
                    "answers": { "type": "array", "minItems": 1, "maxItems": 5, "items": answers },
                }),
            ),
        );
        Self {
            table: HostServiceTable::new(USER_QUESTIONS_V1, vec![begin, poll]),
            broker,
            tickets: HashMap::new(),
        }
    }
}

impl HostService for UserQuestionService {
    fn table(&self) -> &HostServiceTable {
        &self.table
    }

    fn dispatch(
        &mut self,
        method: &str,
        args: &Value,
        context: &ServiceCallContext,
    ) -> Result<Value, ServiceError> {
        let broker = self.broker.as_ref().ok_or_else(|| {
            ServiceError::new(
                "E_SERVICE_UNAVAILABLE",
                "user questions require an interactive desktop session",
            )
        })?;
        let (Some(session), Some(request)) = (&context.session_id, &context.tool_request_id) else {
            return Err(ServiceError::new(
                "E_SERVICE_DENIED",
                "user question has no active tool call",
            ));
        };
        match method {
            "begin" => {
                if self.tickets.contains_key(request) {
                    return Err(ServiceError::new(
                        "E_SERVICE_DENIED",
                        "tool call already has a question batch",
                    ));
                }
                let questions: Vec<Question> = serde_json::from_value(args["questions"].clone())
                    .map_err(|error| ServiceError::args("/questions", error.to_string()))?;
                let mut ids = HashSet::new();
                for (index, question) in questions.iter().enumerate() {
                    if question.id.trim().is_empty()
                        || question.question.trim().is_empty()
                        || !ids.insert(&question.id)
                    {
                        return Err(ServiceError::args(
                            format!("/questions/{index}"),
                            "blank question or duplicate question ID",
                        ));
                    }
                    let mut labels = HashSet::new();
                    if question.options.iter().any(|option| {
                        option.label.trim().is_empty() || !labels.insert(&option.label)
                    }) {
                        return Err(ServiceError::args(
                            format!("/questions/{index}/options"),
                            "blank or duplicate option label",
                        ));
                    }
                }
                let ticket = broker.begin(session, questions)?;
                self.tickets.insert(request.clone(), ticket);
                Ok(json!({}))
            }
            "poll" => {
                let ticket = self.tickets.get(request).ok_or_else(|| {
                    ServiceError::new("E_CANCELLED", "user question is no longer active")
                })?;
                broker.poll(session, *ticket)
            }
            _ => Err(ServiceError::new(
                "E_SERVICE_METHOD",
                "unknown question method",
            )),
        }
    }

    fn release_tool_call(&mut self, request_id: &str) {
        if let (Some(broker), Some(ticket)) = (&self.broker, self.tickets.remove(request_id)) {
            broker.release(ticket);
        }
    }
}

impl Drop for UserQuestionService {
    fn drop(&mut self) {
        if let Some(broker) = &self.broker {
            for (_, ticket) in self.tickets.drain() {
                broker.release(ticket);
            }
        }
    }
}
