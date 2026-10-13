create table accounts (
    id integer primary key,
    name text not null,
    nickname text,
    balance_cents integer not null default 0
);
