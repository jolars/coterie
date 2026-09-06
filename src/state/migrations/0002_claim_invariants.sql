CREATE UNIQUE INDEX one_claim_per_operation
    ON claims (operation_id);

CREATE UNIQUE INDEX one_assignment_per_claim
    ON assignments (claim_id);

CREATE UNIQUE INDEX one_active_assignment_per_agent
    ON assignments (agent_id)
    WHERE completed_at IS NULL;
