// /login splash — see pietro.md §14.2.
//
// One button. The IdP owns the credential UI. Clicking "Sign in" navigates
// the browser to /api/auth/login, which 303-redirects to the OIDC authorize
// URL (with PKCE + state + nonce in the pietro_flow cookie — see §10).
//
// We use a real <a href> instead of fetch so the browser follows the 303
// chain naturally. POST or fetch + redirect would break the OIDC dance.

export default function Login() {
  return (
    <main className="min-h-screen flex items-center justify-center bg-zinc-50 dark:bg-zinc-950 text-zinc-900 dark:text-zinc-100 px-4">
      <div className="w-full max-w-sm rounded-2xl border border-zinc-200 dark:border-zinc-800 bg-white dark:bg-zinc-900 p-8 shadow-sm">
        <h1 className="text-2xl font-semibold tracking-tight">Pietro</h1>
        <p className="mt-2 text-sm text-zinc-500 dark:text-zinc-400">
          Sign in to manage your API keys.
        </p>
        <a
          href="/api/auth/login"
          className="mt-6 inline-flex w-full items-center justify-center rounded-lg bg-zinc-900 dark:bg-zinc-100 px-4 py-2 text-sm font-medium text-white dark:text-zinc-900 hover:opacity-90 transition"
        >
          Sign in
        </a>
      </div>
    </main>
  )
}
