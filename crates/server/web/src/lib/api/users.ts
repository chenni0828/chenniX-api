import api from "@/lib/api"

export interface UserConfig {
  id: number
  username: string
  role: number
  status: number
  group: string
  quota: number
  used_quota: number
  request_count?: number
}

export interface CreateUserPayload {
  username: string
  password: string
  role: number
  group: string
  quota: number
}

export interface UpdateUserPayload {
  username: string
  role: number
  status: number
  group: string
  quota: number
}

export const userApi = {
  list: () => api.get<UserConfig[]>("/users").then((r) => r.data),
  create: (data: CreateUserPayload) => api.post("/users", data).then((r) => r.data),
  update: (id: number, data: UpdateUserPayload) =>
    api.put(`/users/${id}`, data).then((r) => r.data),
  delete: (id: number) => api.delete(`/users/${id}`).then((r) => r.data),
  // Admin resets another user's password.
  updatePassword: (id: number, password: string) =>
    api.put(`/users/${id}/password`, { password }).then((r) => r.data),
  // Self-service password change (requires old password).
  updateMyPassword: (oldPassword: string, newPassword: string) =>
    api.put("/me/password", { old_password: oldPassword, new_password: newPassword }).then((r) => r.data),
}
