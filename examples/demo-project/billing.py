"""Small example with explicitly typed and legacy call sites."""

from dataclasses import dataclass


@dataclass(frozen=True)
class Invoice:
    customer: str
    amount_cents: int


class BillingService:
    def charge(self, invoice: Invoice) -> str:
        if invoice.amount_cents <= 0:
            raise ValueError("The amount must be positive")
        return f"Charged {invoice.customer}: {invoice.amount_cents} cents"


def checkout(service: BillingService, customer: str) -> str:
    """This receiver has an explicit type annotation."""
    invoice = Invoice(customer=customer, amount_cents=4200)
    return service.charge(invoice)


def legacy_checkout(service, invoice):
    """This receiver is deliberately outside future typed refactorings."""
    return service.charge(invoice)
