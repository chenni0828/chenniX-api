import axios from 'axios'

const api = axios.create({
  baseURL: '/admin/api',
  withCredentials: true,
})

api.interceptors.response.use(
  (response) => response,
  (error) => {
    if (error.response?.status === 401) {
      const current = window.location.pathname
      if (current !== '/admin/login' && current !== '/setup') {
        window.location.href = '/admin/login'
      }
    }
    return Promise.reject(error)
  }
)

export default api

export const authApi = {
  login: (username: string, password: string) =>
    api.post('/auth/login', { username, password }),
  logout: () => api.post('/auth/logout'),
  me: () => api.get('/auth/me'),
}
