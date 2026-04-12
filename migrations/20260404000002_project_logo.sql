-- Add logo_path column to projects table for auto-detected project logos.
ALTER TABLE projects ADD COLUMN logo_path TEXT;
