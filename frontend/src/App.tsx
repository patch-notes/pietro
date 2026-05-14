import { useEffect, useState, type ReactNode } from 'react'
import {
  BrowserRouter,
  Navigate,
  Route,
  Routes,
  useLocation,
} from 'react-router-dom'
import { ApiError, api, type Me } from './api'
import Dashboard from './pages/Dashboard'
import Login from './pages/Login'
import NewKey from './pages/NewKey'

// Session probe state: undefined while loading, null when unauthenticated,
// Me when logged in. The three-state shape is the simplest thing that lets
// us avoid flashing the login screen during the initial /api/me round trip.
type SessionState = { status: 'loading' } | { status: 'out' } | { status: 'in'; me: Me }

function RequireAuth({ session, children }: { session: SessionState; children: ReactNode }) {
  const location = useLocation()
  if (session.status === 'loading') {
    return null
  }
  if (session.status === 'out') {
    return <Navigate to="/login" replace state={{ from: location }} />
  }
  return <>{children}</>
}

export default function App() {
  const [session, setSession] = useState<SessionState>({ status: 'loading' })

  useEffect(() => {
    let cancelled = false
    api
      .me()
      .then((me) => {
        if (!cancelled) setSession({ status: 'in', me })
      })
      .catch((err) => {
        if (cancelled) return
        if (err instanceof ApiError && err.status === 401) {
          setSession({ status: 'out' })
        } else {
          // Network / unexpected — treat as logged-out so we render
          // something rather than spin forever. The login page is a
          // reasonable fallback: clicking Sign in will retry the whole
          // flow.
          setSession({ status: 'out' })
        }
      })
    return () => {
      cancelled = true
    }
  }, [])

  return (
    <BrowserRouter>
      <Routes>
        <Route path="/login" element={<Login />} />
        <Route
          path="/"
          element={
            <RequireAuth session={session}>
              <Dashboard />
            </RequireAuth>
          }
        />
        <Route
          path="/new"
          element={
            <RequireAuth session={session}>
              <NewKey />
            </RequireAuth>
          }
        />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Routes>
    </BrowserRouter>
  )
}
