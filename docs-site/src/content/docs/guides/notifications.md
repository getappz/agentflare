---
title: Channel notifications
description: Send agent messages and human-in-the-loop alerts to Telegram, Slack, or Discord.
---

`agentflare channel send` posts a message to Telegram, Slack, or Discord, rendered
as a card on each platform (a Slack Block Kit section, a Discord embed), using a
bot token stored in the local encrypted vault (see
[`agentflare vault`](/docs/reference/cli/)).
The supervisor daemon reuses the same path to ping you when an item needs a human — a
go/no-go decision, an unanswerable question, or a CI self-repair cap.

## Setting up a Telegram bot

1. Message [@BotFather](https://t.me/BotFather) on Telegram and run `/newbot`, following
   the prompts to name your bot. It replies with a bot token that looks like
   `123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11`.
2. Store it in the vault — `set` always reads the value from stdin, never a CLI
   argument, so it can't leak into shell history:

   ```bash
   echo "123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11" | agentflare vault set telegram_bot_token
   ```

3. Get the chat id to send to: open a chat with your bot (or add it to a group) and send
   it any message, then fetch:

   ```bash
   curl "https://api.telegram.org/bot<token>/getUpdates"
   ```

   and read `result[].message.chat.id` from the response.
4. Send a test message to confirm it works — `message` is a plain positional argument,
   not a flag:

   ```bash
   agentflare channel send --to telegram --target <chat_id> "test"
   ```

## Getting notified on human-in-loop decisions

Once the bot token above is stored, opt in to the supervisor's own notifications by
storing the same chat id under a second, separate secret:

```bash
echo "<chat_id>" | agentflare vault set telegram_notify_chat_id
```

With that set, the supervisor pings the chat whenever it hands an item to a human —
gated on `needs-decision`, asks a question it can't answer itself, or gives up
self-repairing a PR after hitting the retry cap. Leave the secret unset to keep this
path silent; nothing else changes.

Every one of these pings is a formatted Telegram card (item number, name, and the
reason it's gated). One is richer still: once a PR goes CI-green and is only
waiting on the `status:pr:approved` label, the card also carries an inline
**✅ Approve** button. Tapping it adds the label directly (no need to open GitHub),
and the supervisor's own review sweep picks that label up and merges on its next
tick. The button only works from the chat id stored above.

## Slack and Discord

Same `channel send` command, different platform and secret name
(`Platform::secret_name` in `src/channels.rs`):

- **Slack** — create an app at [api.slack.com/apps](https://api.slack.com/apps), add
  the `chat:write` bot token scope, install it to your workspace, invite the bot to the
  target channel, then `echo "$TOKEN" | agentflare vault set slack_bot_token`.
  `--target` is the channel id (e.g. `C0123456789`).
- **Discord** — create an application and bot at
  [discord.com/developers/applications](https://discord.com/developers/applications),
  invite it to your server with the `Send Messages` permission, then
  `echo "$TOKEN" | agentflare vault set discord_bot_token`. `--target` is the numeric
  channel id (enable Developer Mode, then right-click the channel → Copy Channel ID).

