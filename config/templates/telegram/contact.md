[‌](https://geodineum.com/wp-content/uploads/2026/07/cropped-Geodineum_Logo.png)*⚡ GEODINEUM* — new contact

*{{ content.subject | default(value="Contact Form Submission") | tg_escape }}*

{{ content.body | default(value="No message") | tg_escape }}

*From:* {{ sender.name | default(value="Unknown") | tg_escape }}
{% if sender.email %}*Email:* {{ sender.email | tg_escape }}{% endif %}
{% if sender.phone %}*Phone:* {{ sender.phone | tg_escape }}{% endif %}

*Site:* {{ site_id | tg_escape }}
_{{ timestamp | tg_escape }}_
