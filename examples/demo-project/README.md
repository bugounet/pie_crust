# Demonstration project

Open `billing.py`, search for `charge`, edit the text, and save with Ctrl+S or Cmd+S.

The project contains typed uses and one deliberately untyped use to prepare future refactoring tests. Semantic navigation and transformations are not yet available in the first version.

The Python tests have no external dependencies:

```sh
python -m unittest discover -s tests -v
```

## Reading path

- `Invoice` describes invoice data.
- `BillingService.charge` validates the amount.
- `checkout` uses an explicitly annotated service.
- `legacy_checkout` serves as the unannotated case.

Markdown preview can display this page next to its source code.
