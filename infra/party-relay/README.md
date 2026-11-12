# Własny serwer pokoi (Cloudflare, za darmo)

Serwer jest malutki: przekazuje między znajomymi tylko podsumowania wspólnych meczów (nicki i liczby skrzynek).
Stawia go **jedna osoba, raz**. Reszta tylko wpisuje adres i kod pokoju w aplikacji.
Darmowy plan Cloudflare wystarcza (100 000 zapytań dziennie; otwarta aplikacja robi ok. 4 300 dziennie), bez karty i bez usypiania.

## Wdrożenie krok po kroku

1. Załóż darmowe konto na https://dash.cloudflare.com/sign-up (potwierdź e-mail).
2. Zainstaluj Node.js (wersja LTS) z https://nodejs.org.
3. Pobierz ten folder `infra/party-relay` (albo całe repozytorium) na dysk.
4. Otwórz w nim terminal (w Eksploratorze wpisz `cmd` w pasku adresu) i wpisz po kolei:
   ```
   npm install
   npx wrangler login
   npx wrangler deploy
   ```
   `login` otworzy przeglądarkę – kliknij "Allow". Przy pierwszym `deploy` Cloudflare może poprosić o wybranie
   nazwy subdomeny `workers.dev` – wpisz dowolną.
5. Na końcu zobaczysz adres w stylu `https://lootledger-party-relay.TWOJA-NAZWA.workers.dev`. Skopiuj go.
6. W aplikacji: strona **Party** → **Online room** → wklej adres w "Server address".
7. Naciśnij **New room code** – powstanie kod pokoju. Przekaż znajomym adres serwera i kod.
8. Każdy znajomy wpisuje ten sam adres i kod. Po wspólnym meczu dane łączą się same (albo przycisk **Share now**).

## Uwagi

- Pokój pamięta dane 24 godziny od ostatniej aktywności, w pokoju może być do 16 osób.
- Kto zna kod pokoju, może go używać – nie publikuj go. W razie czego naciśnij "New room code" i podaj nowy.
- Sprawdzenie, że serwer działa: otwórz jego adres w przeglądarce – powinien pokazać krótki tekst.
- Testy: `npm test`; lokalnie: `npx wrangler dev`.
