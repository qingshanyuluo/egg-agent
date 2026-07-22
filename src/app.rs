#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

pub struct App {
    pub input: String,
    pub messages: Vec<Message>,
    pub should_quit: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            input: String::new(),
            messages: vec![Message {
                role: Role::Assistant,
                content: "Welcome to egg-agent! Type a message and press Enter.".to_string(),
            }],
            should_quit: false,
        }
    }

    pub fn submit(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.messages.push(Message {
            role: Role::User,
            content: text,
        });
        // TODO: wire up a real LLM backend here.
        self.messages.push(Message {
            role: Role::Assistant,
            content: "(no model backend connected yet)".to_string(),
        });
        self.input.clear();
    }

    pub fn quit(&mut self) {
        self.should_quit = true;
    }
}
