/**
 * Turn a workflow's declared `input_schema` into fields somebody can fill in.
 *
 * A template that says it needs `{version: string}` should not make the person
 * launching it hand-write `{"version": "v1.4.2"}` — that is a JSON syntax exam
 * standing between them and the one value the pipeline actually wants, and a
 * missed brace fails as a 422 well after the mistake was made.
 *
 * Only the shapes that map cleanly to a control are handled. Anything else —
 * nested objects, arrays, `oneOf`, no schema at all — falls back to the raw
 * JSON editor, which is always correct and never lies about what it supports.
 */

export type FieldKind = "string" | "number" | "integer" | "boolean" | "enum";

export interface SchemaField {
	key: string;
	kind: FieldKind;
	required: boolean;
	title?: string;
	description?: string;
	/** Present when `kind` is `enum`. */
	options?: string[];
	/** Rendered as the control's initial value. */
	initial?: unknown;
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * The fields for a schema, or `null` when it should be edited as raw JSON.
 *
 * `null` and "no fields" are deliberately different answers: a schema with an
 * empty `properties` map is a run that legitimately takes nothing, and showing
 * it a JSON box would invite input the pipeline has no binding for.
 */
export function fieldsFor(schema: unknown): SchemaField[] | null {
	if (!isRecord(schema)) return null;
	if (schema.type !== undefined && schema.type !== "object") return null;
	if (!isRecord(schema.properties)) return null;
	// A schema that admits values outside `properties` cannot be represented by
	// a fixed set of controls without silently dropping them.
	if (schema.additionalProperties === true) return null;

	const required = Array.isArray(schema.required)
		? schema.required.filter((key): key is string => typeof key === "string")
		: [];

	const fields: SchemaField[] = [];
	for (const [key, raw] of Object.entries(schema.properties)) {
		if (!isRecord(raw)) return null;
		const field = fieldFor(key, raw, required.includes(key));
		// One unrepresentable property makes the whole form wrong, not partial:
		// the launcher would post an object missing a key the pipeline binds to.
		if (!field) return null;
		fields.push(field);
	}
	return fields;
}

function fieldFor(
	key: string,
	schema: Record<string, unknown>,
	required: boolean,
): SchemaField | null {
	const common = {
		key,
		required,
		title: typeof schema.title === "string" ? schema.title : undefined,
		description:
			typeof schema.description === "string" ? schema.description : undefined,
		initial: schema.default,
	};

	if (Array.isArray(schema.enum)) {
		const options = schema.enum.filter(
			(value): value is string => typeof value === "string",
		);
		if (options.length !== schema.enum.length) return null;
		return {...common, kind: "enum", options};
	}

	switch (schema.type) {
		case "string":
			return {...common, kind: "string"};
		case "number":
			return {...common, kind: "number"};
		case "integer":
			return {...common, kind: "integer"};
		case "boolean":
			return {...common, kind: "boolean"};
		default:
			return null;
	}
}

/** What a control holds while being edited. */
export type FieldValue = string | boolean;

export function initialValues(fields: SchemaField[]): Record<string, FieldValue> {
	const values: Record<string, FieldValue> = {};
	for (const field of fields) {
		if (field.kind === "boolean") {
			values[field.key] = field.initial === true;
		} else if (field.initial === undefined || field.initial === null) {
			values[field.key] = "";
		} else {
			values[field.key] =
				typeof field.initial === "string"
					? field.initial
					: JSON.stringify(field.initial);
		}
	}
	return values;
}

/**
 * Build the launch payload, refusing anything the server would reject.
 *
 * Optional fields left blank are omitted rather than sent as `""`: a schema
 * that says `version` is a string and optional means "absent or a string", and
 * an empty string is a value, not an absence.
 */
export function buildInputs(
	fields: SchemaField[],
	values: Record<string, FieldValue>,
): {inputs: Record<string, unknown>} | {error: string} {
	const inputs: Record<string, unknown> = {};
	for (const field of fields) {
		const raw = values[field.key];
		const label = field.title ?? field.key;

		if (field.kind === "boolean") {
			inputs[field.key] = raw === true;
			continue;
		}

		const text = typeof raw === "string" ? raw.trim() : "";
		if (text === "") {
			if (field.required) return {error: `${label} is required.`};
			continue;
		}

		if (field.kind === "number" || field.kind === "integer") {
			const parsed = Number(text);
			if (!Number.isFinite(parsed)) {
				return {error: `${label} must be a number.`};
			}
			if (field.kind === "integer" && !Number.isInteger(parsed)) {
				return {error: `${label} must be a whole number.`};
			}
			inputs[field.key] = parsed;
			continue;
		}

		inputs[field.key] = text;
	}
	return {inputs};
}

/** Blank means "no inputs", not "invalid JSON". */
export function parseJson(text: string): {value: unknown} | {error: string} {
	if (text.trim() === "") return {value: null};
	try {
		return {value: JSON.parse(text)};
	} catch (error) {
		return {error: error instanceof Error ? error.message : "invalid JSON"};
	}
}
