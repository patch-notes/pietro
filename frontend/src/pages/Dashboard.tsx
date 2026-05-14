import { useEffect, useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { ApiError, api, type ApiKey } from '../api'

// / — the dashboard. Lists the current user's keys with a Revoke action
// per active row, and a Create button. See pietro.md §14.2.
//
// Design notes:
//  - The backend's GET /api/keys returns ALL keys for the user, revoked
//    or not (the query has no IS NULL filter). We render both, but dim
//    revoked rows and drop the action on them. This is honest about
//    what the contract returns.
//  - On revoke we mutate optimistically: the row flips to "revoked" in
//    place. On failure we refetch to repair the view.

type KeyRow = ApiKey & { _busy?: boolean }

export default function Dashboard() {
  const [keys, setKeys] = useState<KeyRow[] | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [email, setEmail] = useState<string>('')
  const navigate = useNavigate()

  async function refresh() {
    try {
      const [me, list] = await Promise.all([api.me(), api.listKeys()])
      setEmail(me.email)
      setKeys(list)
      setError(null)
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) {
        navigate('/login', { replace: true })
        return
      }
      setError(e instanceof Error ? e.message : 'failed to load')
    }
  }

  useEffect(() => {
    let cancelled = false
    Promise.all([api.me(), api.listKeys()])
      .then(([me, list]) => {
        if (cancelled) return
        setEmail(me.email)
        setKeys(list)
      })
      .catch((e: unknown) => {
        if (cancelled) return
        if (e instanceof ApiError && e.status === 401) {
          navigate('/login', { replace: true })
          return
        }
        setError(e instanceof Error ? e.message : 'failed to load')
      })
    return () => {
      cancelled = true
    }
  }, [navigate])

  async function onRevoke(id: string) {
    // Optimistic: flip in place. On failure, refetch.
    setKeys((prev) =>
      prev
        ? prev.map((k) =>
            k.id === id
              ? { ...k, _busy: true, revoked_at: new Date().toISOString() }
              : k,
          )
        : prev,
    )
    try {
      await api.revokeKey(id)
      setKeys((prev) =>
        prev ? prev.map((k) => (k.id === id ? { ...k, _busy: false } : k)) : prev,
      )
    } catch (e) {
      // Repair the view from the server.
      await refresh()
      setError(e instanceof Error ? e.message : 'revoke failed')
    }
  }

  async function onLogout() {
    try {
      await api.logout()
    } finally {
      navigate('/login', { replace: true })
    }
  }

  return (
    <main className="min-h-screen bg-zinc-50 dark:bg-zinc-950 text-zinc-900 dark:text-zinc-100">
      <header className="max-w-3xl mx-auto px-6 pt-10 pb-6 flex items-baseline justify-between gap-4">
        <h1 className="text-2xl font-semibold tracking-tight">Pietro</h1>
        <div className="flex items-center gap-3 text-sm">
          {email && <span className="text-zinc-500 dark:text-zinc-400">{email}</span>}
          <button
            onClick={onLogout}
            className="text-zinc-500 dark:text-zinc-400 hover:text-zinc-900 dark:hover:text-zinc-100 transition"
          >
            Sign out
          </button>
        </div>
      </header>

      <section className="max-w-3xl mx-auto px-6">
        <div className="flex items-center justify-between mb-4">
          <h2 className="text-lg font-medium">Your keys</h2>
          <Link
            to="/new"
            className="inline-flex items-center rounded-lg bg-zinc-900 dark:bg-zinc-100 px-3 py-1.5 text-sm font-medium text-white dark:text-zinc-900 hover:opacity-90 transition"
          >
            New key
          </Link>
        </div>

        {error && (
          <div
            role="alert"
            className="mb-4 rounded-lg border border-red-200 dark:border-red-900/50 bg-red-50 dark:bg-red-950/30 px-4 py-3 text-sm text-red-900 dark:text-red-200"
          >
            {error}
          </div>
        )}

        {keys === null ? (
          <p className="text-sm text-zinc-500 dark:text-zinc-400">Loading…</p>
        ) : keys.length === 0 ? (
          <div className="rounded-xl border border-dashed border-zinc-300 dark:border-zinc-700 p-10 text-center">
            <p className="text-zinc-500 dark:text-zinc-400">
              No keys yet. Mint your first one.
            </p>
            <Link
              to="/new"
              className="mt-4 inline-flex items-center rounded-lg bg-zinc-900 dark:bg-zinc-100 px-3 py-1.5 text-sm font-medium text-white dark:text-zinc-900 hover:opacity-90 transition"
            >
              New key
            </Link>
          </div>
        ) : (
          <ul className="divide-y divide-zinc-200 dark:divide-zinc-800 rounded-xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900">
            {keys.map((k) => (
              <KeyListItem key={k.id} k={k} onRevoke={onRevoke} />
            ))}
          </ul>
        )}
      </section>
    </main>
  )
}

function KeyListItem({
  k,
  onRevoke,
}: {
  k: KeyRow
  onRevoke: (id: string) => void
}) {
  const revoked = k.revoked_at !== null
  return (
    <li
      className={
        'px-4 py-3 flex items-center justify-between gap-4 ' +
        (revoked ? 'opacity-50' : '')
      }
    >
      <div className="min-w-0">
        <div className="flex items-center gap-2">
          <span className="font-medium truncate">{k.label}</span>
          <span className="text-xs rounded bg-zinc-100 dark:bg-zinc-800 px-1.5 py-0.5 text-zinc-600 dark:text-zinc-300">
            {k.service_id}
          </span>
          {revoked && (
            <span className="text-xs rounded bg-red-100 dark:bg-red-950/50 px-1.5 py-0.5 text-red-700 dark:text-red-300">
              revoked
            </span>
          )}
        </div>
        <div className="mt-0.5 text-xs text-zinc-500 dark:text-zinc-400 font-mono">
          {k.prefix}…{k.last4}
        </div>
      </div>
      {!revoked && (
        <button
          onClick={() => {
            if (confirm(`Revoke key "${k.label}"? This cannot be undone.`)) {
              onRevoke(k.id)
            }
          }}
          disabled={k._busy}
          className="text-sm text-red-600 dark:text-red-400 hover:text-red-800 dark:hover:text-red-300 disabled:opacity-50 transition"
        >
          Revoke
        </button>
      )}
    </li>
  )
}
