ALTER TABLE notification_deliveries ADD COLUMN receipt_state TEXT NOT NULL DEFAULT 'pending'
    CHECK (receipt_state IN ('pending', 'received', 'legacy'));
ALTER TABLE notification_deliveries ADD COLUMN received_at INTEGER;

-- Older notices have no delivery ID, so their consumption cannot be proved.
UPDATE notification_deliveries SET receipt_state = 'legacy';
