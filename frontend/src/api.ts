// Tiny fetch wrapper for Pietro's REST API.
//
// Contract:
//   - All endpoints live under /api and return JSON on success.
//   - All errors come back as `{ "error": { "code", "message" } }`
//     with a non-2xx status (see src/errors.rs on the backend).
//   - 401 from any call means "not logged in" → caller should redirect
//     to /login. We do NOT auto-redirect here so callers can decide
//     (e.g. /api/me probing the session on mount shouldn't redirect
//     if we're already rendering /login).
//
// Keep this file boring. If it needs to grow, ask why first.

export type ApiErrorBody = {
  error: { code: string; message: string }
}

export class ApiError extends Error {
  status: number
  code: string
  constructor(status: number, code: string, message: string) {
    super(message)
    this.status = status
    this.code = code
  }
}

async function request<T>(
  method: string,
  path: string,
  body?: unknown,
): Promise<T> {
  const init: RequestInit = {
    method,
    credentials: 'same-origin',
    headers: body !== undefined ? { 'content-type': 'application/json' } : {},
    body: body !== undefined ? JSON.stringify(body) : undefined,
  }
  const resp = await fetch(path, init)

  // 204 No Content — return undefined cast to T (callers know).
  if (resp.status === 204) {
    return undefined as T
  }

  // Parse JSON best-effort; both success and error shapes are JSON.
  let parsed: unknown = null
  const text = await resp.text()
  if (text.length > 0) {
    try {
      parsed = JSON.parse(text)
    } catch {
      // Non-JSON body on a non-2xx is still an error.
    }
  }

  if (!resp.ok) {
    const errBody = parsed as ApiErrorBody | null
    const code = errBody?.error?.code ?? 'http_error'
    const message = errBody?.error?.message ?? `HTTP ${resp.status}`
    throw new ApiError(resp.status, code, message)
  }

  return parsed as T
}

// -- typed endpoint helpers -------------------------------------------------

export type Me = {
  user_id: string
  email: string
  display_name: string | null
}

export type Service = {
  id: string
  display_name: string
  description: string | null
}

export type ApiKey = {
  id: string
  service_id: string
  label: string
  prefix: string
  last4: string
  created_at: string
  last_used_at: string | null
  revoked_at: string | null
}

// POST /api/keys returns a flat, minimal shape. Note: the id field on the
// mint response is named `key_id` (not `id`) and the response intentionally
// omits timestamps — see MintKeyResponse in src/routes.rs.
export type MintedKey = {
  key_id: string
  plaintext: string
  prefix: string
  last4: string
  service_id: string
  label: string
}

export const api = {
  me: () => request<Me>('GET', '/api/me'),
  services: () => request<Service[]>('GET', '/api/services'),
  listKeys: () => request<ApiKey[]>('GET', '/api/keys'),
  mintKey: (service_id: string, label: string) =>
    request<MintedKey>('POST', '/api/keys', { service_id, label }),
  revokeKey: (id: string) => request<void>('DELETE', `/api/keys/${id}`),
  logout: () => request<void>('POST', '/api/auth/logout'),
}
