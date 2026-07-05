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
    // 后端 AdminError 返回 { error, code }，前端各处统一读 data.message。
    // 在此把 error 字段映射到 message，避免每处都改读取字段。
    if (error.response?.data && typeof error.response.data === 'object') {
      const data = error.response.data as Record<string, unknown>
      if (data.message === undefined && typeof data.error === 'string') {
        data.message = data.error
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
