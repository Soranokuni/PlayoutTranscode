/**
 * API token plumbing (T1-1 / F-02).
 *
 * The service requires `X-Api-Token` on every `/api/**` call except the health
 * endpoints once `server.api_token` is configured. The token is entered by the
 * operator and kept in `sessionStorage`, so it is scoped to the tab and never
 * persists to disk.
 */

const STORAGE_KEY = 'playout-transcode.apiToken'

let token: string = readStoredToken()

/** Set by `apiFetch` when the service answers 401; watched by the UI shell. */
let authRequiredListeners: Array<(required: boolean) => void> = []
let authRequired = false

function readStoredToken(): string {
  try {
    return sessionStorage.getItem(STORAGE_KEY) ?? ''
  } catch {
    // Private mode or blocked storage: hold the token in memory only.
    return ''
  }
}

export function getApiToken(): string {
  return token
}

export function setApiToken(value: string) {
  token = value.trim()
  try {
    if (token) sessionStorage.setItem(STORAGE_KEY, token)
    else sessionStorage.removeItem(STORAGE_KEY)
  } catch {
    /* memory-only fallback */
  }
  if (token) setAuthRequired(false)
}

export function isAuthRequired(): boolean {
  return authRequired
}

export function onAuthRequired(fn: (required: boolean) => void) {
  authRequiredListeners.push(fn)
  fn(authRequired)
  return () => {
    authRequiredListeners = authRequiredListeners.filter((f) => f !== fn)
  }
}

function setAuthRequired(required: boolean) {
  if (authRequired === required) return
  authRequired = required
  for (const fn of authRequiredListeners) fn(required)
}

/**
 * `fetch` for API calls: attaches the token and flags a 401 so the shell can
 * prompt for one. Use this instead of bare `fetch` for anything under `/api`.
 */
export async function apiFetch(path: string, init: RequestInit = {}): Promise<Response> {
  const headers = new Headers(init.headers ?? {})
  if (token) headers.set('X-Api-Token', token)
  const res = await fetch(path, { ...init, headers })
  if (res.status === 401) setAuthRequired(true)
  return res
}

/**
 * `EventSource` cannot set headers, so the SSE stream carries the token as a
 * query parameter — the one place the server accepts it that way.
 */
export function eventSourceUrl(path: string): string {
  if (!token) return path
  const sep = path.includes('?') ? '&' : '?'
  return `${path}${sep}token=${encodeURIComponent(token)}`
}
