//! Native T3 desktop links use its registered single-instance URI handler.
use super::*;

impl ProjectView {
    pub(super) fn open_in_t3(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(thread) = self.chat.as_ref().map(|chat| chat.thread_id.clone()) else {
            return;
        };
        if let Some(identity) = &self.identity {
            cx.open_url(&identity.thread_url(&thread));
            return;
        }
        #[cfg(target_os = "linux")]
        {
            if self.opening_t3 {
                return;
            }
            let Some(client) = self.native.model_client() else {
                return;
            };
            self.opening_t3 = true;
            let generation = self.generation;
            cx.spawn_in(window, async move |this, cx| {
                let identity = cx
                    .background_executor()
                    .spawn(async move { client.server_identity() })
                    .await;
                let _ = this.update_in(cx, |this, _, cx| {
                    if this.generation != generation || !this.opening_t3 {
                        return;
                    }
                    this.opening_t3 = false;
                    if !this.active
                        || this
                            .chat
                            .as_ref()
                            .is_none_or(|chat| chat.thread_id != thread)
                    {
                        return;
                    }
                    match identity {
                        Ok(identity) => {
                            cx.open_url(&identity.thread_url(&thread));
                            this.identity = Some(identity);
                        }
                        Err(error) => {
                            this.error = Some(format!("Could not open this thread in T3: {error}"))
                        }
                    }
                    cx.notify();
                });
            })
            .detach();
            cx.notify();
        }
        #[cfg(not(target_os = "linux"))]
        let _ = window;
    }
}
