import { ref } from 'vue'

export function useTauri() {
  const invoke = ref(window.__TAURI__?.core?.invoke)

  const safeInvoke = async (cmd, args = {}) => {
    if (!invoke.value) {
      throw new Error('Tauri invoke API not available')
    }
    return await invoke.value(cmd, args)
  }

  // Tauri v2 对话框 API
  const openDialog = async (options = {}) => {
    const dialog = window.__TAURI__?.dialog
    if (!dialog) {
      throw new Error('Tauri dialog API not available')
    }

    // Tauri v2 使用不同的 API
    if (dialog.open) {
      return await dialog.open({
        directory: options.directory || false,
        multiple: options.multiple || false,
        title: options.title || '选择'
      })
    }

    throw new Error('Dialog open method not available')
  }

  return {
    invoke: safeInvoke,
    openDialog
  }
}
