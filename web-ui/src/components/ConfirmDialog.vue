<template>
  <!--
    The service requires `X-Confirm-Destructive: yes` on stop and retry-all
    precisely so that a client asks the operator first. The UI set the header
    automatically and acted on a single click, which on a wall monitor during a
    bulk ingest is one mis-click away from killing every in-flight encode.

    Multi-tier confirmation per NOTEBOOKLM_PLAYOUT_UI_UX §1(5): a distinct
    danger colour, and the cancel button focused rather than the dangerous one.
  -->
  <Teleport to="body">
    <div v-if="open" class="cd-backdrop" @click.self="$emit('cancel')">
      <div
        ref="panelRef"
        class="cd-panel"
        role="alertdialog"
        aria-modal="true"
        :aria-labelledby="titleId"
        :aria-describedby="bodyId"
      >
        <h2 :id="titleId" class="cd-title">{{ title }}</h2>
        <p :id="bodyId" class="cd-body">{{ body }}</p>
        <p v-if="detail" class="cd-detail">{{ detail }}</p>
        <!-- For the choice that belongs *inside* the decision, not before it:
             whether a permanent delete also bins the media file. -->
        <div v-if="$slots.extra" class="cd-extra">
          <slot name="extra" />
        </div>
        <div class="cd-actions">
          <button ref="cancelRef" class="btn cd-cancel" @click="$emit('cancel')">Cancel</button>
          <button class="btn cd-confirm" :disabled="busy" @click="$emit('confirm')">
            {{ busy ? 'Working…' : confirmLabel }}
          </button>
        </div>
      </div>
    </div>
  </Teleport>
</template>

<script setup lang="ts">
import { ref, watch, nextTick, useId, onUnmounted } from 'vue'

const props = defineProps<{
  open: boolean
  title: string
  body: string
  /** Second line, for the consequence an operator may not have in mind. */
  detail?: string
  confirmLabel: string
  busy?: boolean
}>()

const emit = defineEmits<{
  confirm: []
  cancel: []
}>()

const titleId = useId()
const bodyId = useId()
const cancelRef = ref<HTMLButtonElement | null>(null)
const panelRef = ref<HTMLElement | null>(null)
let opener: HTMLElement | null = null

/**
 * Esc and Tab are handled at the document, not on the panel: the panel-level
 * `@keydown.esc` only fired while focus was inside it, so a click on the
 * backdrop's edge left Esc dead, and Tab walked straight out of the "modal"
 * into the page behind it.
 */
function onKeydown(e: KeyboardEvent) {
  if (!props.open) return
  if (e.key === 'Escape') {
    e.preventDefault()
    emit('cancel')
    return
  }
  if (e.key !== 'Tab' || !panelRef.value) return
  const focusable = Array.from(
    panelRef.value.querySelectorAll<HTMLElement>(
      'button:not([disabled]), input:not([disabled]), select:not([disabled]), [href], [tabindex]:not([tabindex="-1"])',
    ),
  )
  if (!focusable.length) return
  const first = focusable[0]!
  const last = focusable[focusable.length - 1]!
  const active = document.activeElement
  if (e.shiftKey && (active === first || !panelRef.value.contains(active))) {
    e.preventDefault()
    last.focus()
  } else if (!e.shiftKey && (active === last || !panelRef.value.contains(active))) {
    e.preventDefault()
    first.focus()
  }
}

// Focus lands on Cancel, never on the destructive button: a stray Return must
// not be the thing that stops a bulk ingest. On close it goes back to the
// control that opened the dialog.
watch(
  () => props.open,
  async (open) => {
    if (open) {
      opener = document.activeElement as HTMLElement | null
      document.addEventListener('keydown', onKeydown)
      await nextTick()
      cancelRef.value?.focus()
    } else {
      document.removeEventListener('keydown', onKeydown)
      opener?.focus?.()
      opener = null
    }
  },
  { immediate: true },
)

onUnmounted(() => document.removeEventListener('keydown', onKeydown))
</script>

<style scoped>
.cd-backdrop {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.6);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 1000;
}

.cd-panel {
  background: var(--bg-panel);
  border: 1px solid var(--accent-crimson);
  border-radius: var(--radius-base);
  padding: 20px 22px;
  max-width: 460px;
  box-shadow: 0 16px 48px rgba(0, 0, 0, 0.55);
}

.cd-title {
  font-size: 15px;
  font-weight: 600;
  color: var(--accent-crimson);
  margin-bottom: 10px;
}

.cd-body {
  font-size: 13px;
  line-height: 1.55;
  color: var(--text-primary);
}

.cd-detail {
  font-size: 12px;
  line-height: 1.55;
  color: var(--text-secondary);
  margin-top: 8px;
}

.cd-extra {
  margin: 12px 0 0;
  padding: 10px 12px;
  border: 1px solid var(--border-subtle);
  border-radius: var(--radius-base);
  font-size: 13px;
}
.cd-actions {
  display: flex;
  justify-content: flex-end;
  gap: 10px;
  margin-top: 18px;
}

.cd-cancel {
  padding: 6px 18px;
  font-size: 13px;
}

.cd-confirm {
  padding: 6px 18px;
  font-size: 13px;
  background: var(--accent-crimson);
  border-color: var(--accent-crimson);
  color: #fff;
  font-weight: 600;
}

.cd-confirm:disabled {
  opacity: 0.6;
  cursor: not-allowed;
}
</style>
