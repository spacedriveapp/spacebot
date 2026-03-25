import createClient from "openapi-fetch";
import type { paths } from "./schema";

let baseUrl = "";

export function setServerUrl(url: string) {
    baseUrl = url;
}

function getClient() {
    return createClient<paths>({
        baseUrl: baseUrl ? `${baseUrl}/api` : "/api",
        headers: getAuthHeaders(),
    });
}

function getAuthHeaders(): Record<string, string> {
    const token = localStorage.getItem("spacebot_auth_token");
    return token ? { Authorization: `Bearer ${token}` } : {};
}

// Re-export the typed client for direct use
export { getClient };
export type { paths };
