use super::*;
use std::collections::VecDeque;

struct Script {
    expected: VecDeque<(RpcRequest, Result<RpcResponse, SupervisorError>)>,
}

impl Transport for Script {
    async fn request(
        &mut self,
        request: RpcRequest,
    ) -> Result<RpcResponse, SupervisorError> {
        let (expected, response) =
            self.expected.pop_front().expect("unexpected RPC");
        assert_eq!(request, expected);
        response
    }
}

fn identity(run_id: RunId, agent_id: AgentId) -> RpcResponse {
    serde_json::from_value(serde_json::json!({
        "result":"identity", "run_id":run_id, "channel":"agent",
        "agent":{"id":agent_id,"name":"planner","role":"planner","state":"running"}
    })).unwrap()
}

#[tokio::test]
async fn poll_drains_empty_pages_and_keeps_inbox_cursor_independent() {
    let run_id = RunId::generate();
    let agent_id = AgentId::generate();
    let cursor = PollCursor {
        run_id,
        agent_id,
        progress: Some("before".into()),
        inbox: 7,
    };
    let mut client = Client::new(Script {
        expected: VecDeque::from([
            (RpcRequest::Whoami, Ok(identity(run_id, agent_id))),
            (
                RpcRequest::Progress {
                    after: cursor.progress.clone(),
                    limit: 100,
                    wait_seconds: 5,
                },
                Ok(RpcResponse::Progress {
                    page: ProgressPage {
                        run_id,
                        changes: vec![],
                        next_cursor: "filtered".into(),
                        has_more: true,
                        timed_out: false,
                    },
                }),
            ),
            (
                RpcRequest::Progress {
                    after: Some("filtered".into()),
                    limit: 100,
                    wait_seconds: 0,
                },
                Ok(RpcResponse::Progress {
                    page: ProgressPage {
                        run_id,
                        changes: vec![],
                        next_cursor: "caught-up".into(),
                        has_more: false,
                        timed_out: false,
                    },
                }),
            ),
            (
                RpcRequest::Inbox { after: 7 },
                Ok(RpcResponse::Inbox {
                    messages: vec![],
                    next_cursor: 7,
                }),
            ),
        ]),
    });
    let page = client
        .poll(Poll {
            cursor: Some(cursor),
            wait_seconds: Some(5),
            include_progress: None,
        })
        .await
        .unwrap();
    assert_eq!(page.cursor.progress.as_deref(), Some("caught-up"));
    assert_eq!(page.cursor.inbox, 7);
    assert!(!page.has_more);
    assert!(client.transport.expected.is_empty());
}

fn message(sequence: u64) -> MessageSummary {
    MessageSummary {
        id: MessageId::generate(),
        sequence,
        sender: None,
        body: format!("Message {sequence}"),
        created_at: 1,
        acknowledged: false,
    }
}

#[tokio::test]
async fn partial_handling_never_acknowledges_an_unhandled_message() {
    let messages = vec![message(1), message(2), message(3)];
    let operation_id = OperationId::generate();
    let mut client = Client::new(Script {
        expected: VecDeque::from([
            (
                RpcRequest::Inbox { after: 0 },
                Ok(RpcResponse::Inbox {
                    messages: messages.clone(),
                    next_cursor: 3,
                }),
            ),
            (
                RpcRequest::InboxAcknowledge {
                    operation_id,
                    through: 1,
                },
                Ok(RpcResponse::InboxAcknowledged {
                    operation_id,
                    acknowledged_through: 1,
                    acknowledged_count: 1,
                }),
            ),
            (
                RpcRequest::Inbox { after: 0 },
                Ok(RpcResponse::Inbox {
                    messages: messages.clone(),
                    next_cursor: 3,
                }),
            ),
        ]),
    });
    client
        .handled(Handled {
            operation_id,
            message_ids: vec![messages[0].id],
        })
        .await
        .unwrap();
    let failure = client
        .handled(Handled {
            operation_id: OperationId::generate(),
            message_ids: vec![messages[2].id],
        })
        .await
        .unwrap_err();
    assert!(matches!(
        failure,
        SupervisorError::Rejected {
            code: RpcFailureCode::Conflict,
            ..
        }
    ));
    assert!(client.transport.expected.is_empty());
}

#[tokio::test]
async fn uncertain_mutations_keep_identical_arguments_and_reject_changes() {
    let operation_id = OperationId::generate();
    let request = RpcRequest::InboxAcknowledge {
        operation_id,
        through: 3,
    };
    let mut client = Client::new(Script {
        expected: VecDeque::from([
            (
                request.clone(),
                Err(SupervisorError::RpcTimeout {
                    action: "test response",
                }),
            ),
            (
                request.clone(),
                Ok(RpcResponse::InboxAcknowledged {
                    operation_id,
                    acknowledged_through: 3,
                    acknowledged_count: 3,
                }),
            ),
        ]),
    });
    assert!(client.request(request, Some(operation_id)).await.is_err());
    assert!(
        client
            .request(
                RpcRequest::InboxAcknowledge {
                    operation_id,
                    through: 4
                },
                Some(operation_id)
            )
            .await
            .is_err()
    );
    assert!(client.retry(operation_id).await.is_ok());
    assert!(client.transport.expected.is_empty());
}

#[tokio::test]
async fn bounded_drain_returns_continuation_and_always_inspects_inbox() {
    let run_id = RunId::generate();
    let agent_id = AgentId::generate();
    let mut expected =
        VecDeque::from([(RpcRequest::Whoami, Ok(identity(run_id, agent_id)))]);
    for index in 0..MAX_PROGRESS_PAGES {
        expected.push_back((
            RpcRequest::Progress {
                after: (index > 0).then(|| index.to_string()),
                limit: 100,
                wait_seconds: 0,
            },
            Ok(RpcResponse::Progress {
                page: ProgressPage {
                    run_id,
                    changes: vec![],
                    next_cursor: (index + 1).to_string(),
                    has_more: true,
                    timed_out: false,
                },
            }),
        ));
    }
    let mut messages = vec![message(1), message(2), message(3)];
    messages[0].acknowledged = true;
    expected.push_back((
        RpcRequest::Inbox { after: 0 },
        Ok(RpcResponse::Inbox {
            messages: messages.clone(),
            next_cursor: 3,
        }),
    ));
    let mut client = Client::new(Script { expected });
    let page = client
        .poll(Poll {
            cursor: None,
            wait_seconds: None,
            include_progress: None,
        })
        .await
        .unwrap();
    assert!(page.has_more);
    assert_eq!(page.cursor.progress, Some(MAX_PROGRESS_PAGES.to_string()));
    assert_eq!(page.cursor.inbox, 1);
    assert_eq!(page.messages, messages[1..]);
    assert!(client.transport.expected.is_empty());
}

#[tokio::test]
async fn timeout_still_reads_messages_and_failed_poll_does_not_consume_a_checkpoint()
 {
    let run_id = RunId::generate();
    let agent_id = AgentId::generate();
    let cursor = PollCursor {
        run_id,
        agent_id,
        progress: Some("saved".into()),
        inbox: 2,
    };
    let mut expected = VecDeque::new();
    for attempt in 0..2 {
        expected.extend([
            (RpcRequest::Whoami, Ok(identity(run_id, agent_id))),
            (
                RpcRequest::Progress {
                    after: cursor.progress.clone(),
                    limit: 100,
                    wait_seconds: 5,
                },
                Ok(RpcResponse::Progress {
                    page: ProgressPage {
                        run_id,
                        changes: vec![],
                        next_cursor: "later".into(),
                        has_more: false,
                        timed_out: true,
                    },
                }),
            ),
            (
                RpcRequest::Inbox { after: 2 },
                if attempt == 0 {
                    Err(SupervisorError::RpcTimeout { action: "inbox" })
                } else {
                    Ok(RpcResponse::Inbox {
                        messages: vec![message(3)],
                        next_cursor: 3,
                    })
                },
            ),
        ]);
    }
    let mut client = Client::new(Script { expected });
    assert!(
        client
            .poll(Poll {
                cursor: Some(cursor.clone()),
                wait_seconds: Some(5),
                include_progress: None
            })
            .await
            .is_err()
    );
    let page = client
        .poll(Poll {
            cursor: Some(cursor),
            wait_seconds: Some(5),
            include_progress: None,
        })
        .await
        .unwrap();
    assert!(page.timed_out);
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.cursor.inbox, 2);
    assert!(client.transport.expected.is_empty());
}

#[tokio::test]
async fn retry_capacity_never_evicts_uncertain_operations() {
    let mut expected = VecDeque::new();
    let requests: Vec<_> = (0..MAX_RETRIES)
        .map(|_| {
            let operation_id = OperationId::generate();
            let request = RpcRequest::InboxAcknowledge {
                operation_id,
                through: 1,
            };
            expected.push_back((
                request.clone(),
                Err(SupervisorError::RpcTimeout {
                    action: "test response",
                }),
            ));
            (operation_id, request)
        })
        .collect();
    expected.push_back((
        requests[0].1.clone(),
        Ok(RpcResponse::InboxAcknowledged {
            operation_id: requests[0].0,
            acknowledged_through: 1,
            acknowledged_count: 1,
        }),
    ));
    let new_id = OperationId::generate();
    let new_request = RpcRequest::InboxAcknowledge {
        operation_id: new_id,
        through: 1,
    };
    expected.push_back((
        new_request.clone(),
        Ok(RpcResponse::InboxAcknowledged {
            operation_id: new_id,
            acknowledged_through: 1,
            acknowledged_count: 0,
        }),
    ));
    let mut client = Client::new(Script { expected });
    for (id, request) in &requests {
        assert!(client.request(request.clone(), Some(*id)).await.is_err());
    }
    assert!(
        client
            .request(new_request.clone(), Some(new_id))
            .await
            .is_err()
    );
    client.retry(requests[0].0).await.unwrap();
    client.request(new_request, Some(new_id)).await.unwrap();
    assert!(client.retry(requests[0].0).await.is_err());
    assert!(
        client
            .mutations
            .iter()
            .any(|saved| saved.id == requests[1].0)
    );
    assert!(client.transport.expected.is_empty());
}
