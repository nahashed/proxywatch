INSERT INTO snapshot(id, data) VALUES (1, ?1)
ON CONFLICT(id) DO UPDATE SET data = excluded.data;
