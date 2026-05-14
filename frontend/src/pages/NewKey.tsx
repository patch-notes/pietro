import { useEffect, useState } from 'react'
import { Link, useNavigate } from 'react-router-dom'
import { ApiError, api, type MintedKey, type Service } from '../api'

// /new — mint a key. See pietro.md §14.2 + §11.2 (plaintext-once).
//
// Two phases:
//   1. Form: pick a service, name the key, submit.
//   2. Reveal: show plaintext exactly once with a big "this will not be
//      shown again" banner, a Copy button, and an explicit "I've saved
//      it" button that returns to /. We deliberately do NOT auto-navigate
//      away on success — the whole point of plaintext-once is that the
//      user must actively acknowledge they captured the value.

export default function NewKey() {
  const [services, setServices] = useState<Service[] | null>(null)
  const [serviceId, setServiceId] = useState<string>('')
  const [label, setLabel] = useState<string>('')
  const [error, setError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)
  const [minted, setMinted] = useState<MintedKey | null>(null)
  const [copied, setCopied] = useState(false)
  const navigate = useNavigate()

  useEffect(() => {
    let cancelled = false
    api
      .services()
      .then((list) => {
        if (cancelled) return
        setServices(list)
        if (list.length > 0) setServiceId(list[0].id)
      })
      .catch((e: unknown) => {
        if (cancelled) return
        if (e instanceof ApiError && e.status === 401) {
          navigate('/login', { replace: true })
          return
        }
        setError(e instanceof Error ? e.message : 'failed to load services')
      })
    return () => {
      cancelled = true
    }
  }, [navigate])

  async function onSubmit(e: React.FormEvent) {
    e.preventDefault()
    setError(null)
    const trimmed = label.trim()
    if (!trimmed) {
      setError('Label is required.')
      return
    }
    if (!serviceId) {
      setError('Pick a service.')
      return
    }
    setSubmitting(true)
    try {
      const result = await api.mintKey(serviceId, trimmed)
      setMinted(result)
    } catch (err) {
      if (err instanceof ApiError && err.status === 401) {
        navigate('/login', { replace: true })
        return
      }
      // Includes 409 key_already_exists — the API error message is
      // already user-facing, so just surface it.
      setError(err instanceof Error ? err.message : 'failed to mint key')
    } finally {
      setSubmitting(false)
    }
  }

  async function onCopy() {
    if (!minted) return
    try {
      await navigator.clipboard.writeText(minted.plaintext)
      setCopied(true)
      setTimeout(() => setCopied(false), 2000)
    } catch {
      // Clipboard API can fail (insecure context, permission) — fall
      // back to selecting the text so the user can ⌘C / Ctrl+C.
      const el = document.getElementById('plaintext-value')
      if (el) {
        const range = document.createRange()
        range.selectNodeContents(el)
        const sel = window.getSelection()
        sel?.removeAllRanges()
        sel?.addRange(range)
      }
    }
  }

  // -- render --------------------------------------------------------------

  if (minted) {
    return (
      <main className="min-h-screen bg-zinc-50 dark:bg-zinc-950 text-zinc-900 dark:text-zinc-100">
        <div className="max-w-2xl mx-auto px-6 py-10">
          <h1 className="text-2xl font-semibold tracking-tight">Your new key</h1>

          <div
            role="alert"
            className="mt-6 rounded-lg border border-amber-300 dark:border-amber-700/50 bg-amber-50 dark:bg-amber-950/30 px-4 py-3 text-sm text-amber-900 dark:text-amber-200"
          >
            <strong className="font-semibold">Copy this value now.</strong>{' '}
            This is the only time it will ever be shown. If you lose it,
            you must revoke the key and mint a new one.
          </div>

          <div className="mt-4 rounded-xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900 p-4">
            <div className="text-xs text-zinc-500 dark:text-zinc-400 mb-1">
              {minted.label}{' '}
              <span className="ml-1 rounded bg-zinc-100 dark:bg-zinc-800 px-1.5 py-0.5">
                {minted.service_id}
              </span>
            </div>
            <code
              id="plaintext-value"
              className="block w-full break-all rounded bg-zinc-100 dark:bg-zinc-800 p-3 font-mono text-sm select-all"
            >
              {minted.plaintext}
            </code>
            <div className="mt-3 flex items-center gap-3">
              <button
                onClick={onCopy}
                className="inline-flex items-center rounded-lg bg-zinc-900 dark:bg-zinc-100 px-3 py-1.5 text-sm font-medium text-white dark:text-zinc-900 hover:opacity-90 transition"
              >
                {copied ? 'Copied!' : 'Copy'}
              </button>
              <Link
                to="/"
                className="text-sm text-zinc-500 dark:text-zinc-400 hover:text-zinc-900 dark:hover:text-zinc-100 transition"
              >
                I've saved it — back to keys
              </Link>
            </div>
          </div>
        </div>
      </main>
    )
  }

  return (
    <main className="min-h-screen bg-zinc-50 dark:bg-zinc-950 text-zinc-900 dark:text-zinc-100">
      <div className="max-w-md mx-auto px-6 py-10">
        <Link
          to="/"
          className="text-sm text-zinc-500 dark:text-zinc-400 hover:text-zinc-900 dark:hover:text-zinc-100 transition"
        >
          ← Back
        </Link>
        <h1 className="mt-4 text-2xl font-semibold tracking-tight">
          Mint a new key
        </h1>

        <form onSubmit={onSubmit} className="mt-6 space-y-4">
          <div>
            <label htmlFor="service" className="block text-sm font-medium mb-1">
              Service
            </label>
            <select
              id="service"
              value={serviceId}
              onChange={(e) => setServiceId(e.target.value)}
              disabled={services === null || submitting}
              className="block w-full rounded-lg border border-zinc-300 dark:border-zinc-700 bg-white dark:bg-zinc-900 px-3 py-2 text-sm"
            >
              {services === null && <option>Loading…</option>}
              {services?.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.display_name}
                </option>
              ))}
            </select>
            {services !== null && services.length === 0 && (
              <p className="mt-1 text-xs text-zinc-500 dark:text-zinc-400">
                The operator hasn't configured any services.
              </p>
            )}
          </div>

          <div>
            <label htmlFor="label" className="block text-sm font-medium mb-1">
              Label
            </label>
            <input
              id="label"
              type="text"
              maxLength={128}
              value={label}
              onChange={(e) => setLabel(e.target.value)}
              disabled={submitting}
              placeholder="e.g. laptop-2026"
              className="block w-full rounded-lg border border-zinc-300 dark:border-zinc-700 bg-white dark:bg-zinc-900 px-3 py-2 text-sm"
            />
            <p className="mt-1 text-xs text-zinc-500 dark:text-zinc-400">
              A short name so you can identify the key later.
            </p>
          </div>

          {error && (
            <div
              role="alert"
              className="rounded-lg border border-red-200 dark:border-red-900/50 bg-red-50 dark:bg-red-950/30 px-4 py-3 text-sm text-red-900 dark:text-red-200"
            >
              {error}
            </div>
          )}

          <button
            type="submit"
            disabled={submitting || services === null || services.length === 0}
            className="inline-flex w-full items-center justify-center rounded-lg bg-zinc-900 dark:bg-zinc-100 px-4 py-2 text-sm font-medium text-white dark:text-zinc-900 hover:opacity-90 transition disabled:opacity-50"
          >
            {submitting ? 'Minting…' : 'Mint key'}
          </button>
        </form>
      </div>
    </main>
  )
}
