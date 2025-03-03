from datetime import datetime
from typing import Optional

from daneel.data.postgres.models import (
    ChatMessageEntity,
    HourlyUsageEntity,
    OrganizationEntity,
)


async def has_token_credits(org: OrganizationEntity) -> bool:
    return True


async def token_credits_remaining(org: OrganizationEntity) -> Optional[int]:
    """
    Returns the number of token credits remaining for the organization.
    None means no limit, so remaining count is undefined.
    """
    return None


async def mark_message(msg: ChatMessageEntity, credit_usage: int):
    HourlyUsageEntity.account_usage(
        msg.session.feature,
        datetime.now().replace(minute=0, second=0, microsecond=0),
        "llm_usage",
        credit_usage,  # credit usage is stored in db in "cents"
    )
