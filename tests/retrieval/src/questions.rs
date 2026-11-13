//! The U07 labeled question set: the single source of truth for what the
//! retrieval benchmark measures.
//!
//! Every question carries a natural-language `text`, its `language`
//! (`en` / `sr` — Serbian, Latin script), a `category` describing the
//! retrieval phenomenon it probes, the `channel` its correct original
//! lives in (`email` / `chat`), and either a labeled [`TargetSpec`] (the
//! one correct original, inserted once into the synthetic corpus) or
//! nothing (`no-answer` probes whose terms must match nothing).
//!
//! Design rules, enforced by the harness self-check:
//!
//! - `exact-term` queries are a single distinctive term that appears in
//!   exactly one corpus message (the labeled original).
//! - `no-answer` query tokens appear in no corpus message at all; any
//!   non-empty result page is false confidence.
//! - Confusers are deliberate distractor messages (all-terms or
//!   topic-adjacent) inserted alongside the background noise so the
//!   measured ranking discriminates rather than merely matches.
//!
//! The set satisfies the user-success plan's U07 floor of "at least 30
//! labeled questions across the chosen sources", reported per category
//! and language.

/// One labeled correct original: inserted verbatim into the corpus,
/// identified by `(source, external_id)`, with its row id captured at
/// insert time.
pub struct TargetSpec {
    /// Legacy source namespace: `mbox` (email) or `whatsapp` / `telegram`
    /// (chat exports).
    pub source: &'static str,
    pub external_id: &'static str,
    /// Display author (email `Name <addr>` or chat handle).
    pub author: &'static str,
    /// RFC 3339 UTC.
    pub sent_at: &'static str,
    /// Conversation grouping within the source; empty for single-mailbox
    /// email.
    pub chat_id: &'static str,
    /// Searchable body. For email this includes the subject as the first
    /// line, matching how mail ingest populates the searchable text.
    pub body: &'static str,
}

/// One deliberate distractor message scattered through the background.
pub struct ConfuserSpec {
    pub source: &'static str,
    pub author: &'static str,
    pub sent_at: &'static str,
    pub chat_id: &'static str,
    pub body: &'static str,
}

/// One labeled benchmark question.
pub struct Question {
    pub id: &'static str,
    /// The natural-language query, exactly as the benchmark searches it.
    pub text: &'static str,
    pub language: &'static str,
    pub category: &'static str,
    pub channel: &'static str,
    /// The one correct original; `None` for `no-answer` probes.
    pub target: Option<TargetSpec>,
    /// Deliberate distractors inserted near the background noise.
    pub confusers: &'static [ConfuserSpec],
    /// Design intent, echoed into the JSON report.
    pub note: &'static str,
}

/// Convenience constructors keep the table below readable.
const fn email_target(
    external_id: &'static str,
    author: &'static str,
    sent_at: &'static str,
    body: &'static str,
) -> TargetSpec {
    TargetSpec {
        source: "mbox",
        external_id,
        author,
        sent_at,
        chat_id: "",
        body,
    }
}

const fn chat_target(
    source: &'static str,
    external_id: &'static str,
    author: &'static str,
    sent_at: &'static str,
    chat_id: &'static str,
    body: &'static str,
) -> TargetSpec {
    TargetSpec {
        source,
        external_id,
        author,
        sent_at,
        chat_id,
        body,
    }
}

const fn confuser(
    source: &'static str,
    author: &'static str,
    sent_at: &'static str,
    chat_id: &'static str,
    body: &'static str,
) -> ConfuserSpec {
    ConfuserSpec {
        source,
        author,
        sent_at,
        chat_id,
        body,
    }
}

/// The question set. Counts: 36 questions, 30 answerable, 6 no-answer;
/// en 18 / sr 18; email 15 / chat 15 among the answerable.
pub const QUESTIONS: &[Question] = &[
    // -----------------------------------------------------------------
    // exact-term: one distinctive term, exactly one corpus occurrence.
    // -----------------------------------------------------------------
    Question {
        id: "q-exact-1",
        text: "telemetry",
        language: "en",
        category: "exact-term",
        channel: "email",
        target: Some(email_target(
            "q-exact-1@retrieval.example",
            "Ops Bot <ops@corp.example>",
            "2026-03-14T09:12:00Z",
            "Subject: Sensor swap complete\n\nAttaching the telemetry calibration report from yesterday's sensor swap. Values look nominal across all racks.",
        )),
        confusers: &[
            confuser(
                "mbox",
                "Ops Bot <ops@corp.example>",
                "2025-11-02T08:00:00Z",
                "",
                "Subject: Pressure sensors\n\nCalibration of the pressure sensors is done, values logged to the dashboard.",
            ),
        ],
        note: "single rare term, unique in the corpus; the calibration confuser shares the co-occurring word but not the needle",
    },
    Question {
        id: "q-exact-2",
        text: "jalapeño",
        language: "en",
        category: "exact-term",
        channel: "chat",
        target: Some(chat_target(
            "telegram",
            "q-exact-2@retrieval.example",
            "garden_gang",
            "2026-05-21T18:40:00Z",
            "garden",
            "the jalapeño plants survived the frost, harvesting tonight before the rain",
        )),
        confusers: &[],
        note: "single rare non-ASCII term in a chat export",
    },
    Question {
        id: "q-exact-3",
        text: "otpremnica",
        language: "sr",
        category: "exact-term",
        channel: "email",
        target: Some(email_target(
            "q-exact-3@retrieval.example",
            "Skladiste <skladiste@firma.example>",
            "2026-04-02T10:05:00Z",
            "Subject: Otpremnica mart\n\nŠaljem otpremnicu za mart, proveri količine pre knjiženja.",
        )),
        confusers: &[
            confuser(
                "mbox",
                "Skladiste <skladiste@firma.example>",
                "2026-03-28T09:30:00Z",
                "",
                "Subject: Predračun\n\nPredračun za mart je poslat, čekamo odgovor pre isporuci.",
            ),
        ],
        note: "single rare Serbian term, unique in the corpus",
    },
    Question {
        id: "q-exact-4",
        text: "kišobran",
        language: "sr",
        category: "exact-term",
        channel: "chat",
        target: Some(chat_target(
            "whatsapp",
            "q-exact-4@retrieval.example",
            "baka",
            "2026-10-11T16:22:00Z",
            "family",
            "ne zaboravi kišobran kod bake, kiša je najavljena celu nedelju",
        )),
        confusers: &[],
        note: "single rare Serbian term with a diacritic, chat export",
    },
    // -----------------------------------------------------------------
    // phrase: all query terms appear verbatim in the target; confusers
    // carry subsets or all terms with wrong context.
    // -----------------------------------------------------------------
    Question {
        id: "q-phrase-1",
        text: "storage migration checklist",
        language: "en",
        category: "phrase",
        channel: "email",
        target: Some(email_target(
            "q-phrase-1@retrieval.example",
            "It Ops <itops@corp.example>",
            "2026-02-09T11:00:00Z",
            "Subject: Photo archive storage migration\n\nThe storage migration for the photo archive is scheduled for the weekend. Checklist attached, with rollback steps at the end.",
        )),
        confusers: &[
            confuser(
                "mbox",
                "It Ops <itops@corp.example>",
                "2024-06-15T10:00:00Z",
                "",
                "Subject: Re: storage migration\n\nAttaching the checklist from the previous storage migration attempt — superseded by the current plan, kept for reference.",
            ),
            confuser(
                "mbox",
                "Records <records@corp.example>",
                "2025-09-01T08:45:00Z",
                "",
                "Subject: Retention policy\n\nArchive storage quotas updated; the review checklist lives in the wiki.",
            ),
        ],
        note: "bm25 must rank the current checklist above an all-terms superseded thread and a two-terms policy mail",
    },
    Question {
        id: "q-phrase-2",
        text: "surf lessons friday",
        language: "en",
        category: "phrase",
        channel: "chat",
        target: Some(chat_target(
            "telegram",
            "q-phrase-2@retrieval.example",
            "maja_surf",
            "2026-07-03T12:30:00Z",
            "holidays",
            "surf lessons booked for friday morning at the east beach, bring your own wetsuit",
        )),
        confusers: &[
            confuser(
                "telegram",
                "maja_surf",
                "2026-07-01T19:10:00Z",
                "holidays",
                "surf forecast looks poor, maybe lessons move to the weekend",
            ),
            confuser(
                "telegram",
                "dule",
                "2026-07-02T20:00:00Z",
                "holidays",
                "friday drinks at the usual place?",
            ),
        ],
        note: "all-terms target against two-terms and one-term chat distractors",
    },
    Question {
        id: "q-phrase-3",
        text: "potvrđen termin novembar",
        language: "sr",
        category: "phrase",
        channel: "email",
        target: Some(email_target(
            "q-phrase-3@retrieval.example",
            "Ordinacija <info@ordinacija.example>",
            "2026-10-20T08:15:00Z",
            "Subject: Potvrda termina\n\nPotvrđen termin kod stomatologa 12. novembar u 10:30. Molimo javite se ako ne možete.",
        )),
        confusers: &[
            confuser(
                "mbox",
                "Ordinacija <info@ordinacija.example>",
                "2026-09-30T09:00:00Z",
                "",
                "Subject: Podsetnik\n\nPodsetnik: zakazan termin za kontrolu, novembar ili decembar po dogovoru.",
            ),
            confuser(
                "mbox",
                "Ordinacija <info@ordinacija.example>",
                "2026-10-01T14:20:00Z",
                "",
                "Subject: Otkazivanje\n\nOtkazan zakazani termin — molim potvrdu novog termina.",
            ),
        ],
        note: "all three query terms verbatim in the target; confusers share termin/novembar but never all terms",
    },
    Question {
        id: "q-phrase-4",
        text: "kafa subota ujutru",
        language: "sr",
        category: "phrase",
        channel: "chat",
        target: Some(chat_target(
            "whatsapp",
            "q-phrase-4@retrieval.example",
            "mila",
            "2026-01-17T21:05:00Z",
            "friends",
            "kafa kod mene subota ujutru oko deset, donesite kolače",
        )),
        confusers: &[
            confuser(
                "whatsapp",
                "mila",
                "2026-01-15T18:40:00Z",
                "friends",
                "kafa posle posla u četvrtak?",
            ),
            confuser(
                "whatsapp",
                "steve",
                "2026-01-16T12:00:00Z",
                "friends",
                "subota je rođendan mame, spremam tortu",
            ),
        ],
        note: "everyday Serbian chat phrasing, all terms verbatim in the target",
    },
    // -----------------------------------------------------------------
    // paraphrase: shared meaning, no shared rare vocabulary. FTS is
    // expected to miss; this measures the honest gap the user-success
    // plan warns about, not a defect to fix inside this benchmark.
    // -----------------------------------------------------------------
    Question {
        id: "q-para-1",
        text: "who repaired the heating",
        language: "en",
        category: "paraphrase",
        channel: "email",
        target: Some(email_target(
            "q-para-1@retrieval.example",
            "Building <building@corp.example>",
            "2026-01-28T07:55:00Z",
            "Subject: Radiator fixed\n\nThe radiator in the bedroom is fixed — the plumber replaced the valve this morning and the flat is warm again.",
        )),
        confusers: &[
            confuser(
                "mbox",
                "Building <building@corp.example>",
                "2026-01-05T09:00:00Z",
                "",
                "Subject: Heating bill\n\nThe heating bill for november is available in the tenant portal.",
            ),
        ],
        note: "no query content word appears in the target; the heating-bill confuser is a topic-adjacent trap for the vector leg",
    },
    Question {
        id: "q-para-2",
        text: "car battery died",
        language: "en",
        category: "paraphrase",
        channel: "chat",
        target: Some(chat_target(
            "whatsapp",
            "q-para-2@retrieval.example",
            "vladimir",
            "2026-03-08T07:20:00Z",
            "family",
            "the engine would not crank this morning, neighbour gave me a jump and it started",
        )),
        confusers: &[
            confuser(
                "whatsapp",
                "vladimir",
                "2026-03-05T19:30:00Z",
                "family",
                "car pooling for the friday trip, two seats free",
            ),
        ],
        note: "meaning match without vocabulary match; the car-pooling confuser shares the topic word the query uses",
    },
    Question {
        id: "q-para-3",
        text: "problem sa grejanjem",
        language: "sr",
        category: "paraphrase",
        channel: "email",
        target: Some(email_target(
            "q-para-3@retrieval.example",
            "Uprava <uprava@zgrada.example>",
            "2025-12-04T10:10:00Z",
            "Subject: Radijator\n\nRadijator u spavaćoj sobi je zamenjen, stan je konačno topao.",
        )),
        confusers: &[],
        note: "Serbian paraphrase: query about the heating problem, target reports the repair",
    },
    Question {
        id: "q-para-4",
        text: "kolika je plata ovog meseca",
        language: "sr",
        category: "paraphrase",
        channel: "chat",
        target: Some(chat_target(
            "telegram",
            "q-para-4@retrieval.example",
            "sanja",
            "2026-06-25T17:45:00Z",
            "work",
            "isplate su stigle na račun, proverila sam stanje",
        )),
        confusers: &[],
        note: "salary question answered by a payments-arrived message; no shared rare tokens",
    },
    // -----------------------------------------------------------------
    // inflection: the key query term is a morphological variant of the
    // target's token (refund/refunding, genitive vs nominative…). FTS
    // tokens differ; character n-gram overlap is the vector leg's job.
    // -----------------------------------------------------------------
    Question {
        id: "q-inflect-1",
        text: "refunding the theater tickets",
        language: "en",
        category: "inflection",
        channel: "email",
        target: Some(email_target(
            "q-inflect-1@retrieval.example",
            "Box Office <tickets@theater.example>",
            "2026-04-18T15:00:00Z",
            "Subject: Refund issued\n\nA refund was issued for both theater tickets. It should arrive within 3 to 5 business days.",
        )),
        confusers: &[
            confuser(
                "mbox",
                "Box Office <tickets@theater.example>",
                "2025-10-10T12:00:00Z",
                "",
                "Subject: Refund policy\n\nTicket refund policy: no refunds within 48 hours of the performance.",
            ),
        ],
        note: "refunding vs refund; the policy confuser shares the stem and competes for the vector leg",
    },
    Question {
        id: "q-inflect-2",
        text: "cancelled trains",
        language: "en",
        category: "inflection",
        channel: "chat",
        target: Some(chat_target(
            "telegram",
            "q-inflect-2@retrieval.example",
            "rail_alerts",
            "2026-02-11T06:30:00Z",
            "alerts",
            "all trains are canceled until further notice, take the replacement bus from platform 3",
        )),
        confusers: &[],
        note: "cancelled vs canceled — doubled consonant, strong character trigram overlap",
    },
    Question {
        id: "q-inflect-3",
        text: "rezervaciju apartmana",
        language: "sr",
        category: "inflection",
        channel: "email",
        target: Some(email_target(
            "q-inflect-3@retrieval.example",
            "Apartmani JadrAN <info@jadran.example>",
            "2026-05-06T09:40:00Z",
            "Subject: Potvrda\n\nVaša rezervacija apartmana je potvrđena za jul, broj rezervacije 2214.",
        )),
        confusers: &[
            confuser(
                "mbox",
                "Apartmani JadrAN <info@jadran.example>",
                "2026-04-20T11:25:00Z",
                "",
                "Subject: Potvrda poslata\n\nPotvrda rezervacije je poslata mejlom, proverite spam folder.",
            ),
        ],
        note: "accusative rezervaciju vs nominative rezervacija; the confirmation confuser shares the stem",
    },
    Question {
        id: "q-inflect-4",
        text: "dnevni red sastanka",
        language: "sr",
        category: "inflection",
        channel: "chat",
        target: Some(chat_target(
            "telegram",
            "q-inflect-4@retrieval.example",
            "sekretarica",
            "2026-09-15T13:05:00Z",
            "office",
            "šaljem dnevni red za sastanak u petak, pročitajte pre pocetka",
        )),
        confusers: &[],
        note: "genitive sastanka vs nominative sastanak — a form users type constantly",
    },
    // -----------------------------------------------------------------
    // typo: one or two edits in the key term. The diacritic-less sr
    // variant documents the well-known no-diacritics typing problem.
    // -----------------------------------------------------------------
    Question {
        id: "q-typo-1",
        text: "waranty",
        language: "en",
        category: "typo",
        channel: "email",
        target: Some(email_target(
            "q-typo-1@retrieval.example",
            "Service <service@shop.example>",
            "2026-08-22T10:30:00Z",
            "Subject: Warranty claim\n\nThe warranty claim for the washing machine is approved; the replacement part is ordered.",
        )),
        confusers: &[
            confuser(
                "mbox",
                "Service <service@shop.example>",
                "2026-08-01T09:00:00Z",
                "",
                "Subject: Warranty extension\n\nWarranty extension offer for the dishwasher, 20 percent off this month.",
            ),
        ],
        note: "missing r; the extension confuser carries the correctly spelled word and fights for the vector leg",
    },
    Question {
        id: "q-typo-2",
        text: "restorant",
        language: "en",
        category: "typo",
        channel: "chat",
        target: Some(chat_target(
            "telegram",
            "q-typo-2@retrieval.example",
            "gastro",
            "2026-10-02T18:15:00Z",
            "food",
            "restaurant recommendation for saturday night: the new thai place by the market",
        )),
        confusers: &[
            confuser(
                "telegram",
                "gastro",
                "2026-09-28T17:00:00Z",
                "food",
                "restaurant week starts monday, bookings open at noon",
            ),
        ],
        note: "restorant vs restaurant — a common fast-typing form",
    },
    Question {
        id: "q-typo-3",
        text: "racun za maj",
        language: "sr",
        category: "typo",
        channel: "email",
        target: Some(email_target(
            "q-typo-3@retrieval.example",
            "Faktura <faktura@firma.example>",
            "2026-06-01T08:00:00Z",
            "Subject: Račun maj\n\nU prilogu račun za maj, ukupno 42.300 dinara. Rok plaćanja 15 dana.",
        )),
        confusers: &[],
        note: "racun without the diacritic vs račun — measured: the default unicode61 tokenizer folds Latin diacritics, so the FTS leg matches it directly",
    },
    Question {
        id: "q-typo-4",
        text: "prijavnca",
        language: "sr",
        category: "typo",
        channel: "chat",
        target: Some(chat_target(
            "whatsapp",
            "q-typo-4@retrieval.example",
            "admin",
            "2026-09-01T09:50:00Z",
            "course",
            "prijavnica za kurs je poslata, polazak u oktobru",
        )),
        confusers: &[],
        note: "transposed letters (prijavnca vs prijavnica)",
    },
    // -----------------------------------------------------------------
    // attachment: the retrieval task behind a file the user once had —
    // finding the message that carried it. Attachment-content search is
    // a separate research question and is deliberately not claimed here.
    // -----------------------------------------------------------------
    Question {
        id: "q-attach-1",
        text: "signed lease pdf",
        language: "en",
        category: "attachment",
        channel: "email",
        target: Some(email_target(
            "q-attach-1@retrieval.example",
            "Agentura <office@agentura.example>",
            "2026-03-30T14:00:00Z",
            "Subject: Signed lease\n\nPlease find the signed lease attached as PDF; the countersigned copy follows by post.",
        )),
        confusers: &[
            confuser(
                "mbox",
                "Agentura <office@agentura.example>",
                "2026-03-10T10:20:00Z",
                "",
                "Subject: Lease renewal\n\nLease renewal terms attached for review; the new rent applies from may.",
            ),
        ],
        note: "all-terms email about the carried file; confuser shares lease/attached but not pdf/signed",
    },
    Question {
        id: "q-attach-2",
        text: "photos from hiking",
        language: "en",
        category: "attachment",
        channel: "chat",
        target: Some(chat_target(
            "telegram",
            "q-attach-2@retrieval.example",
            "planinari",
            "2026-06-14T20:30:00Z",
            "hikes",
            "sending the photos from the hiking trip, 200 shots in the zip",
        )),
        confusers: &[
            confuser(
                "telegram",
                "planinari",
                "2026-06-12T21:00:00Z",
                "hikes",
                "hiking trail map screenshot saved from the group, check the red line",
            ),
        ],
        note: "chat media message; confuser shares hiking and a media context without photos",
    },
    Question {
        id: "q-attach-3",
        text: "potpisani ugovor pdf",
        language: "sr",
        category: "attachment",
        channel: "email",
        target: Some(email_target(
            "q-attach-3@retrieval.example",
            "Pravni <pravni@firma.example>",
            "2026-02-20T11:30:00Z",
            "Subject: Ugovor\n\nU prilogu potpisani ugovor u PDF formatu, arhivirati original.",
        )),
        confusers: &[],
        note: "Serbian all-terms attachment email",
    },
    Question {
        id: "q-attach-4",
        text: "slike sa svadbe",
        language: "sr",
        category: "attachment",
        channel: "chat",
        target: Some(chat_target(
            "whatsapp",
            "q-attach-4@retrieval.example",
            "fotograf",
            "2026-09-20T19:00:00Z",
            "wedding",
            "šaljem slike sa svadbe, sve sto je bilo na stolu je u folderu",
        )),
        confusers: &[],
        note: "Serbian chat media message",
    },
    // -----------------------------------------------------------------
    // author-topic: author term (matched in the indexed author column)
    // plus topic terms.
    // -----------------------------------------------------------------
    Question {
        id: "q-author-1",
        text: "Milan inventory spreadsheet",
        language: "en",
        category: "author-topic",
        channel: "email",
        target: Some(email_target(
            "q-author-1@retrieval.example",
            "Milan Jovanović <milan.jovanovic@corp.example>",
            "2026-07-07T09:25:00Z",
            "Subject: Inventory\n\nUpdated the inventory spreadsheet; new counts are in column D, discrepancies highlighted.",
        )),
        confusers: &[
            confuser(
                "mbox",
                "Ivana Ivić <ivana.ivic@corp.example>",
                "2026-07-06T16:40:00Z",
                "",
                "Subject: Inventory\n\nInventory counts updated in the shared sheet, please review before friday.",
            ),
            confuser(
                "mbox",
                "Milan Jovanović <milan.jovanovic@corp.example>",
                "2026-07-03T13:00:00Z",
                "",
                "Subject: Quarterly\n\nQuarterly numbers attached, nothing alarming.",
            ),
        ],
        note: "author token matches only Milan's rows; topic terms also appear in another author's mail",
    },
    Question {
        id: "q-author-2",
        text: "nina_dev deploy notes",
        language: "en",
        category: "author-topic",
        channel: "chat",
        target: Some(chat_target(
            "telegram",
            "q-author-2@retrieval.example",
            "nina_dev",
            "2026-08-04T22:10:00Z",
            "devops",
            "deploy notes for tonight: read-only mode first, then the migration, then flip",
        )),
        confusers: &[
            confuser(
                "telegram",
                "nina_dev",
                "2026-08-04T18:55:00Z",
                "devops",
                "deploy window moved to 23:00, sorry for the ping",
            ),
        ],
        note: "chat handle plus topic; confuser shares the handle and deploy but never notes",
    },
    Question {
        id: "q-author-3",
        text: "Jelena računi stan",
        language: "sr",
        category: "author-topic",
        channel: "email",
        target: Some(email_target(
            "q-author-3@retrieval.example",
            "Jelena Petrović <jelena.petrovic@corp.example>",
            "2026-05-12T08:45:00Z",
            "Subject: Računi\n\nPlaćeni računi za stan, skenirala sam sve i složila u folder.",
        )),
        confusers: &[],
        note: "Serbian author + topic terms",
    },
    Question {
        id: "q-author-4",
        text: "Zoran treninzi",
        language: "sr",
        category: "author-topic",
        channel: "chat",
        target: Some(chat_target(
            "whatsapp",
            "q-author-4@retrieval.example",
            "Zoran Trener",
            "2026-10-06T18:30:00Z",
            "sport",
            "treninzi se prebacuju na 19h od sledeće nedelje",
        )),
        confusers: &[],
        note: "Serbian chat coach handle + plural topic term",
    },
    // -----------------------------------------------------------------
    // cross-language: query language differs from the original's, with
    // at most a shared proper noun or loanword. Expected to be hard;
    // measured, not assumed.
    // -----------------------------------------------------------------
    Question {
        id: "q-cross-1",
        text: "visa appointment confirmation",
        language: "en",
        category: "cross-language",
        channel: "email",
        target: Some(email_target(
            "q-cross-1@retrieval.example",
            "Ambasada <consular@embassy.example>",
            "2026-04-07T12:00:00Z",
            "Subject: Potvrda termina\n\nPotvrđen termin za vizu u sredu u 9:00, poneti pasoš i potvrdu.",
        )),
        confusers: &[
            confuser(
                "mbox",
                "Ambasada <consular@embassy.example>",
                "2026-03-15T10:00:00Z",
                "",
                "Subject: Podsetnik\n\nAppointment reminders are sent 24 hours ahead, check your spam folder.",
            ),
        ],
        note: "english query, Serbian original; viza/vizu vs visa is the only lexical bridge",
    },
    Question {
        id: "q-cross-2",
        text: "let za Amsterdam sutra",
        language: "sr",
        category: "cross-language",
        channel: "chat",
        target: Some(chat_target(
            "telegram",
            "q-cross-2@retrieval.example",
            "aerodrom_info",
            "2026-11-03T21:00:00Z",
            "travel",
            "Flight to Amsterdam tomorrow 07:40, gate B12 — do not be late",
        )),
        confusers: &[],
        note: "Serbian query, english original; only Amsterdam is shared",
    },
    // -----------------------------------------------------------------
    // no-answer: plausible questions whose terms match nothing. Any
    // non-empty first page is false confidence.
    // -----------------------------------------------------------------
    Question {
        id: "q-none-1",
        text: "parking spot rotation",
        language: "en",
        category: "no-answer",
        channel: "any",
        target: None,
        confusers: &[],
        note: "plausible office topic, absent from the corpus",
    },
    Question {
        id: "q-none-2",
        text: "office plant watering schedule",
        language: "en",
        category: "no-answer",
        channel: "any",
        target: None,
        confusers: &[],
        note: "plausible office topic, absent from the corpus",
    },
    Question {
        id: "q-none-3",
        text: "xylophone lessons",
        language: "en",
        category: "no-answer",
        channel: "any",
        target: None,
        confusers: &[],
        note: "rare-term probe, absent from the corpus",
    },
    Question {
        id: "q-none-4",
        text: "zamena guma",
        language: "sr",
        category: "no-answer",
        channel: "any",
        target: None,
        confusers: &[],
        note: "plausible Serbian topic, absent from the corpus",
    },
    Question {
        id: "q-none-5",
        text: "tečaj pilatesa",
        language: "sr",
        category: "no-answer",
        channel: "any",
        target: None,
        confusers: &[],
        note: "plausible Serbian topic, absent from the corpus",
    },
    Question {
        id: "q-none-6",
        text: "klizanje na jezeru",
        language: "sr",
        category: "no-answer",
        channel: "any",
        target: None,
        confusers: &[],
        note: "plausible Serbian topic, absent from the corpus",
    },
];
