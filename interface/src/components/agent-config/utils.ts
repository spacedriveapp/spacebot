import type {SectionId} from "./types";

export function supportsAdaptiveThinking(modelId: string): boolean {
	const id = modelId.toLowerCase();
	return (
		id.includes("opus-4-6") ||
		id.includes("opus-4.6") ||
		id.includes("sonnet-4-6") ||
		id.includes("sonnet-4.6")
	);
}

export const isIdentityField = (
	id: SectionId,
): id is "soul" | "identity" | "role" => {
	return id === "soul" || id === "identity" || id === "role";
};

export const getIdentityField = (
	data: {soul: string | null; identity: string | null; role: string | null},
	field: SectionId,
): string | null => {
	if (isIdentityField(field)) {
		return data[field];
	}
	return null;
};
