BEGIN;
GRANT UPDATE (photo) ON users TO wiring_chat_writer;
GRANT UPDATE (sort_order,is_primary) ON photos TO wiring_chat_writer;
COMMIT;
