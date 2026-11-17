ALTER TABLE conversation_library_sections ADD COLUMN display_order INTEGER NOT NULL DEFAULT 0;

WITH ordered_sections AS MATERIALIZED (
    SELECT id, ROW_NUMBER() OVER (PARTITION BY project_id ORDER BY name_key, id) - 1 AS display_order
    FROM conversation_library_sections
)
UPDATE conversation_library_sections
SET display_order = ordered_sections.display_order
FROM ordered_sections
WHERE conversation_library_sections.id = ordered_sections.id;

CREATE INDEX conversation_library_sections_order
ON conversation_library_sections (project_id, display_order, id);
