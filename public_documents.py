"""Editorial content and public link metadata shared by Flask and release builds."""
from glossary import glossary_html
from legal_pages import ACCOUNT_DELETION_HTML, CHILD_SAFETY_HTML, LEGAL_INDEX_HTML, MARKETING_HTML, PRIVACY_HTML, RULES_HTML


PAGES = {
    '/account-deletion': ('Удаление аккаунта и данных', 'Как удалить аккаунт WIRING и что происходит с анкетой и данными.', ACCOUNT_DELETION_HTML),
    '/child-safety': ('Стандарты защиты детей в WIRING', 'WIRING — только для взрослых. Запрет CSAE и CSAM, жалобы и контакт по вопросам защиты детей.', CHILD_SAFETY_HTML),
    '/privacy': ('Политика конфиденциальности', 'Какие персональные данные обрабатывает WIRING и какие у вас есть права.', PRIVACY_HTML),
    '/legal': ('Документы и безопасность', 'Соглашение, конфиденциальность, рассылка, защита детей и удаление аккаунта WIRING.', LEGAL_INDEX_HTML),
    '/rules': ('Пользовательское соглашение', 'Условия использования WIRING. 18+. Ограничения по законодательству РФ.', RULES_HTML),
    '/marketing': ('Согласие на рассылку', 'Отдельное согласие на рекламные сообщения WIRING. Для знакомств не обязательно.', MARKETING_HTML),
    '/glossary': ('Буквы и слова с анкет', 'Простым языком: что значат ASD, СДВГ, ПРЛ и остальные слова на WIRING.', glossary_html()),
}

LANDING_THEMES = {"pastel": "утро", "mist": "день", "dusk": "сумерки", "night": "ночь", "slate": "полночь"}

APPLE_APP_SITE_ASSOCIATION = {'applinks': {'details': [{'appIDs': ['G3T7684N3M.date.wiring.app'], 'components': [{'/': '/api/*', 'exclude': True}, {'/': '/admin*', 'exclude': True}, {'/': '/public/*', 'exclude': True}, {'/': '/uploads/*', 'exclude': True}, {'/': '/.well-known/*', 'exclude': True}, {'/': '/*'}]}]}}
