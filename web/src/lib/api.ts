const API = '/api/v1';

export const csrf: { token: string | null } = { token: null };

export function setCsrf(token: string): void {
	csrf.token = token;
}

export class ApiError extends Error {
	readonly status: number;

	constructor(status: number, message: string) {
		super(message);
		this.name = 'ApiError';
		this.status = status;
	}
}

interface RequestOptions {
	body?: unknown;
	signal?: AbortSignal;
}

function buildHeaders(hasBody: boolean): Record<string, string> {
	const headers: Record<string, string> = { Accept: 'application/json' };
	if (hasBody) headers['Content-Type'] = 'application/json';
	if (csrf.token) headers['X-Toottok-CSRF'] = csrf.token;
	return headers;
}

async function request<T>(method: string, path: string, options: RequestOptions = {}): Promise<T> {
	const hasBody = options.body !== undefined;
	const res = await fetch(`${API}${path}`, {
		method,
		credentials: 'same-origin',
		headers: buildHeaders(hasBody),
		body: hasBody ? JSON.stringify(options.body) : undefined,
		signal: options.signal
	});

	if (!res.ok) {
		let message = `${res.status} ${res.statusText}`;
		try {
			const data = (await res.json()) as { error?: string; detail?: string; title?: string };
			message = data.detail ?? data.error ?? data.title ?? message;
		} catch {
			throw new ApiError(res.status, message);
		}
		throw new ApiError(res.status, message);
	}

	if (res.status === 204) return undefined as T;
	return (await res.json()) as T;
}

export function apiGet<T>(path: string, signal?: AbortSignal): Promise<T> {
	return request<T>('GET', path, { signal });
}

export function apiPost<T>(path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
	return request<T>('POST', path, { body, signal });
}

export function apiPut<T>(path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
	return request<T>('PUT', path, { body, signal });
}

export function apiDelete<T>(path: string, signal?: AbortSignal): Promise<T> {
	return request<T>('DELETE', path, { signal });
}
