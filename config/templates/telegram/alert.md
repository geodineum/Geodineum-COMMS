[‌](https://geodineum.com/wp-content/uploads/2026/07/cropped-Geodineum_Logo.png)*⚡ GEODINEUM*

*{{ content.subject | default(value="System Alert") | tg_escape }}*

{{ content.body | default(value="An alert was triggered.") | tg_escape }}

*Service:* {{ site_id | tg_escape }} · *P{{ message.priority | default(value=3) }}*
_{{ timestamp | tg_escape }}_
