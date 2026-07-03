import api from "@/lib/api"

export interface SetupStatus {
  needs_setup: boolean
}

export interface InitializePayload {
  username: string
  password: string
}

export const setupApi = {
  getStatus: () => api.get<SetupStatus>("/setup/status").then((r) => r.data),
  initialize: (data: InitializePayload) =>
    api.post("/setup/initialize", data).then((r) => r.data),
}
