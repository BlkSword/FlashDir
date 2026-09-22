import { ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'

const volumes = ref([])

export function useVolumes() {
  const refresh = async () => {
    try {
      volumes.value = (await invoke('get_volumes')) || []
    } catch (e) {
      volumes.value = []
    }
  }
  const byLetter = (letter) => volumes.value.find((v) => v.letter === letter) || null
  return { volumes, refresh, byLetter }
}
