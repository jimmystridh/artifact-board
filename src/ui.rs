//! Dioxus server-rendered interface. Browser behavior progressively enhances these components.
use dioxus::prelude::*;
use serde_json::Value;

fn field<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
fn document(title: &str, body: Element) -> String {
    let html = dioxus_ssr::render_element(rsx! {
            head {
                meta { charset: "utf-8" }
                meta { name: "viewport", content: "width=device-width, initial-scale=1" }
                meta { name: "referrer", content: "no-referrer" }
                title { "{title} · Artifact Board" }
                link { rel: "stylesheet", href: "/assets/app.css" }
                script { src: "/assets/app.js", defer: true }
            }
            body { {body} }
    });
    format!("<!DOCTYPE html><html lang=\"en\">{html}</html>")
}

#[component]
fn Mark() -> Element {
    rsx! { a { class: "brand", href: "/", aria_label: "Artifact Board home", span { class: "brand-mark", "a" } "Artifact Board" } }
}

pub fn dashboard(data: Value) -> String {
    let boot = data.to_string();
    let name = field(&data["principal"], "display_name").to_owned();
    let shares = data["shares"].as_array().cloned().unwrap_or_default();
    let total = shares.len();
    let mcp_url = format!("{}/mcp", field(&data, "origin").trim_end_matches('/'));
    let add_command = format!("codex mcp add artifact-board --url {mcp_url}");
    let claude_command =
        format!("claude mcp add --transport http --scope user artifact-board {mcp_url}");
    document(
        "Your workspace",
        rsx! {
            div { id: "app", "data-page": "dashboard", "data-boot": "{boot}",
                header { class: "topbar", Mark {} div { class: "account", span { class: "online-dot" } "{name}" a { class: "text-button", href: "/oauth/connections", "Connected agents" } button { id: "sign-out", class: "text-button", "Sign out" } } }
                main { class: "dashboard",
                    div { class: "workspace-label", span { class: "tiny-square" } "YOUR WORKSPACE" }
                    section { class: "page-heading",
                        div { h1 { "Good work deserves a second look." } p { "Publish an artifact. Share a link. Keep the conversation in context." } }
                        button { class: "button primary", id: "new-share", "+ New share" }
                    }
                    section { class: "workspace-grid",
                        div { class: "shares-panel",
                            div { class: "section-top", h2 { "My shares" span { class: "count", "{total}" } } input { id: "share-search", r#type: "search", placeholder: "Find a share…", aria_label: "Find a share" } }
                            nav { class: "filters", aria_label: "Share status", button { class: "filter active", "data-filter": "all", "All shares" } button { class: "filter", "data-filter": "published", "Published" } button { class: "filter", "data-filter": "archived", "Archived" } }
                            div { id: "share-list",
                                if shares.is_empty() {
                                    div { class: "empty-state", div { class: "empty-icon", "↗" } h3 { "Your next idea starts here" } p { "A report, a website, a work in progress. Give it a home and invite a fresh perspective." } button { class: "button secondary", "data-new-share": "true", "Create your first share" } }
                                }
                                for share in shares {
                                    ShareCard { share }
                                }
                            }
                            p { id: "no-results", class: "hidden subtle", "No shares match your search." }
                        }
                        aside { class: "workspace-aside",
                            section { class: "agent-help", aria_labelledby: "agent-help-title",
                                span { class: "eyebrow", "WORK WITH YOUR AGENT" }
                                h2 { id: "agent-help-title", "Connect your agent" }
                                p { "Let your agent publish artifacts and work with feedback as you." }
                                label { r#for: "agent-client", class: "agent-client-label", "Choose your agent" }
                                select { id: "agent-client",
                                    option { value: "codex", "Codex" }
                                    option { value: "claude-code", "Claude Code" }
                                    option { value: "claude-web", "Claude on the web" }
                                    option { value: "chatgpt-web", "ChatGPT on the web" }
                                }
                                div { "data-agent-guide": "codex",
                                ol { class: "agent-help-steps",
                                    li {
                                        h3 { "Add Artifact Board to Codex" }
                                        div { class: "agent-command",
                                            div { class: "agent-command-bar", span { "TERMINAL" } button { class: "text-button", "data-copy-command": "agent-add-command", aria_label: "Copy add command", "Copy" } }
                                            pre { code { id: "agent-add-command", "{add_command}" } }
                                        }
                                    }
                                    li {
                                        h3 { "Sign in and approve access" }
                                        div { class: "agent-command",
                                            div { class: "agent-command-bar", span { "TERMINAL" } button { class: "text-button", "data-copy-command": "agent-login-command", aria_label: "Copy sign-in command", "Copy" } }
                                            pre { code { id: "agent-login-command", "codex mcp login artifact-board" } }
                                        }
                                        p { "Follow the browser prompt, sign in with your work Microsoft account, and approve the connection. Then open a new Codex session." }
                                    }
                                }
                                    a { class: "agent-guide-link", href: "https://developers.openai.com/codex/mcp", target: "_blank", rel: "noopener noreferrer", "Codex setup guide ↗" }
                                }
                                div { "data-agent-guide": "claude-code", hidden: true,
                                    ol { class: "agent-help-steps",
                                        li { h3 { "Add the server" }
                                            div { class: "agent-command",
                                                div { class: "agent-command-bar", span { "TERMINAL" } button { class: "text-button", "data-copy-command": "agent-claude-command", aria_label: "Copy Claude Code command", "Copy" } }
                                                pre { code { id: "agent-claude-command", "{claude_command}" } }
                                            }
                                        }
                                        li { h3 { "Connect in Claude Code" }
                                            div { class: "agent-command",
                                                div { class: "agent-command-bar", span { "IN CLAUDE CODE" } button { class: "text-button", "data-copy-command": "agent-claude-login", aria_label: "Copy Claude Code sign-in command", "Copy" } }
                                                pre { code { id: "agent-claude-login", "/mcp" } }
                                            }
                                            p { "Open Claude Code, run /mcp, select artifact-board, and follow the browser sign-in. Use your work Microsoft account and approve access." }
                                        }
                                    }
                                    a { class: "agent-guide-link", href: "https://code.claude.com/docs/en/mcp", target: "_blank", rel: "noopener noreferrer", "Claude Code setup guide ↗" }
                                }
                                div { "data-agent-guide": "claude-web", hidden: true,
                                    ol { class: "agent-help-steps",
                                        li { h3 { "Add a custom connector" } p { "In Claude, open Customize → Connectors → + Add → Add custom connector." } }
                                        li { h3 { "Enter the connection details" } p { "Name it Artifact Board and paste the MCP server URL below. Choose OAuth sign-in and Register automatically for the OAuth client." } }
                                        li { h3 { "Sign in and enable it" } p { "Connect with your work Microsoft account and approve access. In your chat, open + → Connectors and enable Artifact Board." } }
                                    }
                                    p { class: "agent-plan-note", "On Team or Enterprise, an owner may need to add the connector in Organization settings → Connectors first." }
                                    a { class: "agent-guide-link", href: "https://support.claude.com/en/articles/11175166-get-started-with-custom-connectors-using-remote-mcp", target: "_blank", rel: "noopener noreferrer", "Claude connector guide ↗" }
                                }
                                div { "data-agent-guide": "chatgpt-web", hidden: true,
                                    ol { class: "agent-help-steps",
                                        li { h3 { "Add a custom MCP server" } p { "In ChatGPT, open Plugins → + → Add custom MCP server." } }
                                        li { h3 { "Enter the connection details" } p { "Name it Artifact Board, paste the MCP server URL below, and choose OAuth with dynamic client registration (DCR). Leave client ID and secret empty." } }
                                        li { h3 { "Connect and install" } p { "Create it as a plugin, complete your work Microsoft sign-in, and install it. In a new chat, type @ and select Artifact Board." } }
                                    }
                                    p { class: "agent-plan-note", "Your workspace must allow custom MCP plugins. Ask an admin if the add option is unavailable." }
                                    a { class: "agent-guide-link", href: "https://chatgpt.com/plugins", target: "_blank", rel: "noopener noreferrer", "Open ChatGPT Plugins ↗" }
                                    a { class: "agent-guide-link", href: "https://developers.openai.com/api/docs/guides/custom-mcp-server", target: "_blank", rel: "noopener noreferrer", "ChatGPT setup guide ↗" }
                                }
                                div { class: "agent-command agent-server-endpoint",
                                    div { class: "agent-command-bar", span { "MCP SERVER URL" } button { class: "text-button", "data-copy-command": "agent-server-url", aria_label: "Copy MCP server URL", "Copy" } }
                                    pre { code { id: "agent-server-url", "{mcp_url}" } }
                                }
                                div { class: "agent-help-prompt", span { "THEN TRY ASKING" } p { "“Publish this HTML file to Artifact Board and send me the review link.”" } }
                                details { class: "agent-help-other",
                                    summary { "Using another agent?" }
                                    p { "Other desktop MCP clients can use the server URL above with OAuth browser sign-in and automatic client registration." }
                                }
                                div { class: "agent-help-links", a { href: "/oauth/connections", "Manage connected agents →" } }
                            }
                            div { class: "supported", h3 { "Bring your own format" } p { span { "01" } "Static websites & mockups" } p { span { "02" } "Markdown documents" } p { span { "03" } "Complete React projects" } }
                            p { class: "fineprint", "Revisions change. The conversation stays." }
                        }
                    }
                    footer { class: "dashboard-footer", "Built for work in progress." span { "ARTIFACT BOARD / 01" } }
                }
                UploadDialog {}
                div { id: "toast", role: "status", class: "toast hidden" }
            }
        },
    )
}

#[component]
fn ShareCard(share: Value) -> Element {
    let id = field(&share, "id");
    let title = field(&share, "title");
    let kind = field(&share, "kind");
    let policy = field(&share, "policy");
    let state = field(&share, "state");
    let token = field(&share, "viewer_token");
    let url = format!("/s/{token}");
    let ready = !share["latest_revision"].is_null() && state == "published";
    let policy_label = if policy == "entra-gated" {
        "Entra sign-in"
    } else {
        "Anyone with the link"
    };
    rsx! { article { class: "share-card", "data-title": "{title}", "data-state": "{state}", "data-share": "{id}",
        div { class: "artifact-icon", if kind == "markdown" { "M↓" } else if kind == "react" { "✳" } else { "〈/〉" } }
        div { class: "share-details", h3 { if ready { a { href: "{url}", "{title}" } } else { "{title}" } } div { class: "share-meta", span { "{kind}" } span { "·" } span { "{policy_label}" } } }
        div { class: "share-controls", span { class: "status-badge", "data-state": "{state}", "{state}" }
            if ready { button { class: "icon-button copy-link", "data-url": "{url}", title: "Copy review link", aria_label: "Copy review link", "↗" } }
            details { class: "share-menu", summary { aria_label: "Share actions", "•••" } div { class: "menu-items",
                if state != "archived" && state != "deleted" { button { "data-revision": "{id}", "data-kind": "{kind}", "data-title": "{title}", "Upload revision" } }
                if state == "published" { button { "data-action": "unpublish", "data-id": "{id}", "Unpublish" } }
                if state == "unpublished" { button { "data-action": "republish", "data-id": "{id}", "Republish" } }
                if state != "archived" && state != "deleted" { button { "data-action": "archive", "data-id": "{id}", "Archive" } }
                if state == "archived" || state == "deleted" { button { "data-action": "restore", "data-id": "{id}", "Restore" } }
                if state != "deleted" { button { class: "danger", "data-action": "delete", "data-id": "{id}", "Delete" } }
            } }
        }
    } }
}

#[component]
fn UploadDialog() -> Element {
    rsx! {
        dialog { id: "upload-dialog", class: "upload-dialog",
            form { id: "upload-form",
                div { class: "dialog-heading", div { span { class: "eyebrow", "MAKE SOMETHING SHAREABLE" } h2 { id: "upload-title", "Publish a new share" } } button { r#type: "button", class: "icon-button", id: "close-upload", aria_label: "Close upload", "×" } }
                label { r#for: "artifact-title", "Title" } input { id: "artifact-title", name: "title", required: true, maxlength: "160", placeholder: "e.g. A better onboarding experience" }
                div { class: "form-columns", div { label { r#for: "artifact-kind", "Content format" } select { id: "artifact-kind", name: "kind", required: true, option { value: "", "Choose a format" } option { value: "static", "Static HTML" } option { value: "markdown", "Markdown" } option { value: "react", "React project" } } } div { id: "policy-field", label { r#for: "artifact-policy", "Who can view?" } select { id: "artifact-policy", name: "policy", required: true, option { value: "", "Choose access" } option { value: "link-only", "Anyone with the link" } option { value: "entra-gated", "Entra sign-in required" } } } }
                label { class: "dropzone", r#for: "artifact-files", span { class: "upload-glyph", "↑" } strong { "Choose a ZIP or files" } span { "HTML + assets, Markdown, or a complete React project" } input { id: "artifact-files", r#type: "file", multiple: true, required: true } }
                p { id: "file-summary", class: "subtle", "No files selected" }
                label { r#for: "entrypoint", "Entrypoint" } input { id: "entrypoint", name: "entrypoint", value: "index.html", required: true } p { class: "fineprint", "Use a relative file path. React projects require package.json and a supported static build." }
                p { id: "upload-error", class: "form-error", role: "alert" } p { id: "upload-progress", class: "subtle", role: "status" }
                div { class: "dialog-footer", span { id: "policy-summary", class: "fineprint", "Choose viewer access before publishing." } button { r#type: "submit", class: "button primary", id: "publish-button", "Publish share ↗" } }
            }
        }
    }
}

pub fn review(data: Value) -> String {
    let boot = data.to_string();
    let title = field(&data["share"], "title").to_owned();
    let content_url = field(&data, "content_url").to_owned();
    let sign_in_url = format!("/login?next=/s/{}", field(&data, "capability"));
    let revision = field(&data["revision"], "id").to_owned();
    let short_revision: String = revision.chars().take(8).collect();
    document(
        &title,
        rsx! {
            div { id: "app", "data-page": "review", "data-boot": "{boot}",
                main { class: "review-layout",
                    section { class: "canvas", aria_label: "Artifact preview",
                        iframe { id: "artifact-frame", src: "{content_url}", title: "{title} preview", "sandbox": "allow-scripts", referrerpolicy: "no-referrer" }
                        div { id: "selection-action", class: "selection-action hidden", button { id: "comment-selection", class: "button primary small", "+ Comment on selection" } }
                    }
                    aside { class: "discussion", id: "discussion-panel", aria_label: "Review panel", hidden: true,
                        div { class: "review-panel-heading", div { span { class: "eyebrow", "REVIEW" } h2 { "{title}" } p { "Revision {short_revision}" } } button { id: "close-review", class: "icon-button", aria_label: "Close review panel", title: "Back to document (Escape)", "×" } }
                        section { class: "revision-history", aria_label: "Revision history",
                            label { r#for: "revision-picker", "Version history" }
                            select { id: "revision-picker", aria_label: "View a revision" }
                            div { id: "earlier-revision", hidden: true,
                                p { "You’re viewing an earlier revision." }
                                a { id: "latest-revision-link", href: "#", "Back to latest →" }
                                if data["is_owner"] == true { button { id: "delete-revision", class: "text-button danger-text", "Delete this revision…" } }
                            }
                        }
                        div { class: "review-panel-links", a { href: "/", "← My shares" } button { id: "copy-review-link", class: "text-button", "Copy link ↗" } }
                        div { class: "review-panel-tools", button { id: "select-element", class: "tool-button", aria_pressed: "false", "⊞ Select element" } button { id: "general-comment", class: "tool-button", "+ General comment" } }
                        div { class: "hide-review-control",
                            button { id: "hide-review", class: "button secondary hide-review",
                                svg { width: "16", height: "16", view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "1.7", "aria-hidden": "true", path { d: "m3 3 18 18M10.6 10.6a2 2 0 0 0 2.8 2.8M9.9 5.2A12 12 0 0 1 12 5c6 0 10 7 10 7a17 17 0 0 1-3.5 4.1M6.1 6.1A20 20 0 0 0 2 12s4 7 10 7a12 12 0 0 0 5.9-1.6" } }
                                "Hide review tools"
                            }
                            p { "Reload the page to show them again." }
                        }
                        p { id: "selection-hint", class: "review-panel-hint", "Highlight text in the page, or select an element." }
                        div { class: "discussion-header", h2 { "Discussion" span { id: "thread-count", class: "count", "0" } } div { class: "tool-group", button { id: "refresh-discussion", class: "icon-button", aria_label: "Refresh discussion", title: "Refresh discussion", "↻" } button { id: "new-comment", class: "icon-button", aria_label: "New general comment", "+" } } }
                        nav { class: "discussion-tabs", aria_label: "Discussion filter", button { class: "thread-filter active", "data-thread-filter": "open", "Open" } button { class: "thread-filter", "data-thread-filter": "unmatched", "Unmatched" } button { class: "thread-filter", "data-thread-filter": "resolved", "Resolved" } button { class: "thread-filter", "data-thread-filter": "archived", "Archived" } }
                        div { id: "thread-list", class: "thread-list", aria_live: "polite" }
                        details { id: "comment-composer-panel", class: "comment-composer-panel",
                            summary { "Leave a comment" }
                        form { id: "comment-form", class: "comment-composer",
                            div { class: "composer-heading", strong { id: "composer-label", "General feedback" } button { r#type: "button", id: "cancel-anchor", class: "text-button", "Clear" } }
                            p { id: "anchor-preview", class: "anchor-preview hidden" }
                            label { r#for: "guest-name", "Your name" } input { id: "guest-name", name: "guest_name", required: true, maxlength: "80", placeholder: "What should we call you?" }
                            p { class: "identity-note", "Guest names are unverified. " a { href: "{sign_in_url}", "Sign in instead" } }
                            label { r#for: "comment-body", class: "sr-only", "Your feedback" } textarea { id: "comment-body", required: true, maxlength: "8000", rows: "3", placeholder: "What’s working? What could be better?" }
                            p { id: "comment-error", class: "form-error", role: "alert" } button { r#type: "submit", class: "button primary", "Post comment ↑" }
                        }
                        }
                    }
                }
                aside { id: "review-intro", class: "review-intro", hidden: true, aria_labelledby: "review-intro-title",
                    button { id: "close-review-intro", class: "icon-button review-intro-close", aria_label: "Dismiss comment introduction", "×" }
                    span { class: "review-intro-eyebrow", "A QUICK HELLO" }
                    h2 { id: "review-intro-title", "Your feedback goes here." }
                    p { "Open comments with the button below. Highlight text or pick an element to comment on something specific." }
                    div { class: "review-intro-actions",
                        button { id: "show-review-intro", class: "button primary", "Show me" }
                        button { id: "dismiss-review-intro", class: "text-button", "Got it" }
                    }
                }
                button { id: "toggle-discussion", class: "review-launcher", aria_label: "Open review panel", aria_expanded: "false", aria_controls: "discussion-panel", title: "Review this document",
                    svg { class: "review-launcher-icon", width: "22", height: "22", view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "1.7", "aria-hidden": "true", path { d: "M20 11.5a7.5 7.5 0 0 1-7.5 7.5H5l-3 3V11.5A7.5 7.5 0 0 1 9.5 4h3A7.5 7.5 0 0 1 20 11.5Z" } path { d: "M7 10h8M7 14h5" } }
                    span { id: "comment-count", class: "review-comment-count", hidden: true, aria_hidden: "true" }
                    span { class: "review-launcher-label", "Review" }
                    span { class: "review-launcher-close", aria_hidden: "true", "×" }
                }
                dialog { id: "review-confirm", class: "review-confirm", aria_labelledby: "review-confirm-title", aria_describedby: "review-confirm-message",
                    h2 { id: "review-confirm-title" }
                    p { id: "review-confirm-message" }
                    p { id: "review-confirm-error", class: "form-error", role: "alert" }
                    div { class: "review-confirm-actions",
                        button { id: "review-confirm-cancel", class: "button secondary", "Cancel" }
                        button { id: "review-confirm-submit", class: "button danger", "Delete" }
                    }
                }
                div { id: "review-hidden-notice", class: "review-hidden-notice", hidden: true, role: "status", tabindex: "-1", "Comments hidden. Reload the page to reveal them again." }
                div { id: "toast", class: "toast hidden", role: "status" }
            }
        },
    )
}

pub fn login(error: Option<&str>) -> String {
    document(
        "Local development sign-in",
        rsx! {
            main { class: "login-page", Mark {} section { class: "login-card", span { class: "eyebrow", "LOCAL DEVELOPMENT" } h1 { "Make room for a fresh perspective." } p { "Sign in with the local publisher fixture to create and review artifacts." } if let Some(message) = error { p { class: "form-error", role: "alert", "{message}" } } form { id: "dev-login", action: "/dev/login", method: "post", button { class: "button primary", r#type: "submit", "Continue as local publisher ↗" } } p { class: "fineprint", "Development only. This fixture is not production authentication." } } }
        },
    )
}

pub fn login_production(error: Option<&str>) -> String {
    document(
        "Sign in",
        rsx! {
            main { class: "login-page", Mark {} section { class: "login-card", span { class: "eyebrow", "A SHARED PERSPECTIVE" } h1 { "Good work gets better together." } p { "Sign in with your organization to publish and manage your artifacts." } if let Some(message) = error { p { class: "form-error", role: "alert", "{message}" } } a { href: "/auth/login", class: "button primary", "Continue with Microsoft ↗" } p { class: "fineprint", "Access is limited to the configured organization and admitted users." } } }
        },
    )
}

/// Complete script tag, injected into sandboxed HTML. Nonce is negotiated by the shell.
pub fn bridge_script(_nonce: &str) -> String {
    format!("<script>{}</script>", include_str!("../assets/bridge.js"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escapes_untrusted_title() {
        let html = dashboard(
            serde_json::json!({"shares":[{"id":"one","title":"<script>alert(1)</script>","state":"draft","kind":"html"}],"principal":{"display_name":"A"}}),
        );
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;") || html.contains("&#60;"), "{html}");
    }
    #[test]
    fn frame_has_opaque_origin() {
        let html = review(
            serde_json::json!({"share":{"title":"Test"},"revision":{"id":"r1"},"content_url":"/content/t/r1/index.html"}),
        );
        assert!(html.contains("sandbox=\"allow-scripts\""));
        assert!(!html.contains("allow-same-origin"));
    }
    #[test]
    fn upload_requires_explicit_policy_and_has_real_components() {
        let html =
            dashboard(serde_json::json!({"shares":[],"principal":{"display_name":"Publisher"}}));
        assert!(html.contains("Artifact Board"));
        assert!(html.contains("id=\"upload-dialog\""));
        assert!(html.contains("Choose access"));
        assert!(html.contains("value=\"link-only\""));
        assert!(html.contains("value=\"entra-gated\""));
        assert!(!html.contains("selected=\"link-only\""));
    }
    #[test]
    fn production_login_has_no_development_fixture() {
        let html = login_production(None);
        assert!(html.contains("/auth/login"));
        assert!(!html.contains("/dev/login"));
        assert!(!html.contains("local publisher"));
    }
}
