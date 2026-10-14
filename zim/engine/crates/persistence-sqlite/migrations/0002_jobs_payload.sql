-- M8 (jobs): kind-specific job targets as opaque JSON text.
--
-- A backup-create job links to its Backup row through that row's
-- job_id (the M3 companion-entity pattern). A restore job has no
-- such row, so it names its target (the backup id being restored)
-- here. Transfer jobs will carry their node pair through the
-- Transfer row (same companion pattern), so the column stays
-- sparse on purpose.

ALTER TABLE jobs ADD COLUMN payload TEXT;
